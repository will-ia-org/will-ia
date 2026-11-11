//! Menções (`@`) em grupo. O WhatsApp só mostra uma menção de verdade quando a
//! mensagem leva o JID da pessoa em `ContextInfo.mentionedJid` **e** o texto
//! tem o token `@<usuário do JID>` no lugar da menção. Grupos novos são
//! endereçados por LID: o JID e o token precisam ser o LID do participante
//! (`@<telefone>` vira texto solto ou "@Unknown user"); grupos antigos usam o
//! telefone. Este módulo é puro: recebe os participantes do grupo e o que o
//! agente pediu, devolve JIDs + tokens e reescreve o texto.

use crate::backend::{GroupAddressing, GroupMembers};

/// O que o agente pediu para mencionar, já normalizado pela ferramenta
/// (telefone, LID ou nome resolvido pelo índice).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionQuery {
    /// Como veio no argumento (erros e busca de `@…` no texto).
    pub raw: String,
    /// Telefone com DDI (só dígitos), se conhecido.
    pub phone: Option<String>,
    /// Parte de usuário do LID (antes de `@lid`), se conhecida.
    pub lid: Option<String>,
    /// Nome do contato, se o pedido foi por nome (acha `@Nome` no texto).
    pub name: Option<String>,
}

/// Menção resolvida contra o grupo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// JID que vai em `mentionedJid` (`<lid>@lid` ou `<tel>@s.whatsapp.net`).
    pub jid: String,
    /// Token que precisa aparecer no texto (`@<usuário>`).
    pub token: String,
    pub query: MentionQuery,
}

/// Parte de usuário de um JID (sem dispositivo `:n`).
pub(crate) fn user_of(jid: &str) -> &str {
    let user = jid.split('@').next().unwrap_or(jid);
    user.split(':').next().unwrap_or(user)
}

/// Resolve cada pedido contra os participantes. `lid_map` = pares
/// `(lid_user, telefone)` conhecidos pela sessão, para casar telefone com LID
/// quando o grupo só informa um dos dois. Erro no primeiro pedido que não
/// está no grupo (a mensagem toda é recusada: melhor do que mandar meia menção).
pub fn resolve(
    members: &GroupMembers,
    lid_map: &[(String, String)],
    queries: Vec<MentionQuery>,
) -> Result<Vec<Mention>, String> {
    let mut out = Vec::with_capacity(queries.len());
    for q in queries {
        // completa telefone ↔ LID pelo mapa da sessão
        let phone = q.phone.clone().or_else(|| {
            q.lid.as_deref().and_then(|l| {
                lid_map
                    .iter()
                    .find(|(lu, _)| lu == l)
                    .map(|(_, p)| p.clone())
            })
        });
        let lid = q.lid.clone().or_else(|| {
            phone.as_deref().and_then(|p| {
                lid_map
                    .iter()
                    .find(|(_, ph)| ph == p)
                    .map(|(lu, _)| lu.clone())
            })
        });
        let found = members.members.iter().find(|m| {
            let by_phone = match (&phone, &m.phone_jid) {
                (Some(p), Some(pj)) => same_phone(p, user_of(pj)),
                _ => false,
            };
            let by_lid = match (&lid, &m.lid_jid) {
                (Some(l), Some(lj)) => user_of(lj) == l,
                _ => false,
            };
            by_phone || by_lid
        });
        let Some(member) = found else {
            return Err(format!(
                "{} não está no grupo (confira com whatsapp_get_group)",
                q.raw
            ));
        };
        let jid = match members.addressing {
            GroupAddressing::Lid => member.lid_jid.as_ref().or(member.phone_jid.as_ref()),
            GroupAddressing::Phone => member.phone_jid.as_ref().or(member.lid_jid.as_ref()),
        }
        .cloned()
        .ok_or_else(|| format!("{}: participante sem JID", q.raw))?;
        let token = format!("@{}", user_of(&jid));
        out.push(Mention {
            jid,
            token,
            query: q,
        });
    }
    Ok(out)
}

/// Mesmo telefone tolerando DDI/DDD faltando de um lado (mínimo 8 dígitos).
fn same_phone(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches('+'), b.trim_start_matches('+'));
    if a == b {
        return true;
    }
    let short = a.len().min(b.len());
    short >= 8 && (a.ends_with(b) || b.ends_with(a))
}

/// Reescreve o texto para que cada menção tenha o seu token: troca `@<como foi
/// pedido>`, `@<telefone>`, `@+<telefone>`, `@<lid>` ou `@<Nome>` pelo token
/// certo; se o texto não cita a pessoa, o token entra no começo.
pub fn apply(text: &str, mentions: &[Mention]) -> String {
    let mut out = text.to_string();
    let mut missing: Vec<&str> = Vec::new();
    for m in mentions {
        let q = &m.query;
        let mut candidates: Vec<String> = vec![format!("@{}", q.raw)];
        if let Some(p) = &q.phone {
            candidates.push(format!("@{p}"));
            candidates.push(format!("@+{p}"));
        }
        if let Some(l) = &q.lid {
            candidates.push(format!("@{l}"));
        }
        if let Some(n) = &q.name {
            candidates.push(format!("@{n}"));
            // nome composto escrito só pelo primeiro nome ("@Ana" para "Ana Silva")
            if let Some(first) = n.split_whitespace().next().filter(|f| *f != n) {
                candidates.push(format!("@{first}"));
            }
        }
        let mut hit = false;
        for c in candidates {
            if c.len() <= 1 {
                continue;
            }
            let (replaced, n) = replace_token(&out, &c, &m.token);
            if n > 0 {
                out = replaced;
                hit = true;
            }
        }
        if !hit {
            missing.push(&m.token);
        }
    }
    if missing.is_empty() {
        out
    } else {
        let lead = missing.join(" ");
        if out.trim().is_empty() {
            lead
        } else {
            format!("{lead} {out}")
        }
    }
}

/// Troca todas as ocorrências de `needle` (sem distinguir maiúsculas) que
/// terminam em fronteira de palavra. Devolve o texto novo e quantas trocou.
fn replace_token(text: &str, needle: &str, token: &str) -> (String, usize) {
    // Mapear índices de `lower` em `text` só é seguro se cada caractere manteve
    // o mesmo tamanho em bytes ao virar minúsculo (ex.: `İ` vira `i̇`, 2→3
    // bytes); senão compara exato.
    let same_layout = text.chars().all(|c| {
        let mut l = c.to_lowercase();
        matches!((l.next(), l.next()), (Some(x), None) if x.len_utf8() == c.len_utf8())
    });
    if !same_layout {
        return replace_exact(text, needle, token);
    }
    let lower = text.to_lowercase();
    let nl = needle.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut n = 0;
    while let Some(pos) = lower[i..].find(&nl) {
        let start = i + pos;
        let end = start + nl.len();
        let boundary = text[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        out.push_str(&text[i..start]);
        if boundary {
            out.push_str(token);
            n += 1;
        } else {
            out.push_str(&text[start..end]);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    (out, n)
}

fn replace_exact(text: &str, needle: &str, token: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut n = 0;
    while let Some(pos) = text[i..].find(needle) {
        let start = i + pos;
        let end = start + needle.len();
        let boundary = text[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        out.push_str(&text[i..start]);
        if boundary {
            out.push_str(token);
            n += 1;
        } else {
            out.push_str(&text[start..end]);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::GroupMember;

    fn group(addr: GroupAddressing) -> GroupMembers {
        GroupMembers {
            addressing: addr,
            members: vec![
                GroupMember {
                    phone_jid: Some("5511911111111@s.whatsapp.net".into()),
                    lid_jid: Some("111222333@lid".into()),
                    is_admin: true,
                },
                GroupMember {
                    phone_jid: None,
                    lid_jid: Some("444555666@lid".into()),
                    is_admin: false,
                },
                GroupMember {
                    phone_jid: Some("5511933333333@s.whatsapp.net".into()),
                    lid_jid: None,
                    is_admin: false,
                },
            ],
        }
    }

    fn q(raw: &str, phone: Option<&str>, lid: Option<&str>, name: Option<&str>) -> MentionQuery {
        MentionQuery {
            raw: raw.into(),
            phone: phone.map(Into::into),
            lid: lid.map(Into::into),
            name: name.map(Into::into),
        }
    }

    #[test]
    fn lid_group_uses_lid_jid_and_token() {
        let g = group(GroupAddressing::Lid);
        let r = resolve(
            &g,
            &[],
            vec![q("5511911111111", Some("5511911111111"), None, None)],
        )
        .unwrap();
        assert_eq!(r[0].jid, "111222333@lid");
        assert_eq!(r[0].token, "@111222333");
        // grupo por telefone prefere o telefone
        let g = group(GroupAddressing::Phone);
        let r = resolve(
            &g,
            &[],
            vec![q("111222333@lid", None, Some("111222333"), None)],
        )
        .unwrap();
        assert_eq!(r[0].jid, "5511911111111@s.whatsapp.net");
        assert_eq!(r[0].token, "@5511911111111");
    }

    #[test]
    fn lid_map_bridges_phone_and_lid() {
        let g = group(GroupAddressing::Lid);
        let map = vec![("444555666".to_string(), "5511922222222".to_string())];
        let r = resolve(
            &g,
            &map,
            vec![q("5511922222222", Some("5511922222222"), None, None)],
        )
        .unwrap();
        assert_eq!(r[0].jid, "444555666@lid");
        // sem o mapa, não acha
        assert!(resolve(
            &g,
            &[],
            vec![q("5511922222222", Some("5511922222222"), None, None)]
        )
        .is_err());
        // participante só com telefone num grupo LID: cai no telefone
        let r = resolve(
            &g,
            &[],
            vec![q("11933333333", Some("11933333333"), None, None)],
        )
        .unwrap();
        assert_eq!(r[0].jid, "5511933333333@s.whatsapp.net");
    }

    #[test]
    fn replace_token_survives_case_changing_byte_length() {
        // `İ` (2 bytes) vira `i̇` (3 bytes): cai na comparação exata sem pânico
        let (out, n) = replace_token("İ @Ana x", "@ana", "@1");
        assert_eq!((out.as_str(), n), ("İ @Ana x", 0));
        let (out, n) = replace_token("İ @Ana x", "@Ana", "@1");
        assert_eq!((out.as_str(), n), ("İ @1 x", 1));
        let (out, n) = replace_token("oi @ANA!", "@ana", "@1");
        assert_eq!((out.as_str(), n), ("oi @1!", 1));
    }

    #[test]
    fn apply_rewrites_or_prepends() {
        let g = group(GroupAddressing::Lid);
        let r = resolve(
            &g,
            &[],
            vec![
                q("Ana", Some("5511911111111"), None, Some("Ana")),
                q("444555666@lid", None, Some("444555666"), None),
            ],
        )
        .unwrap();
        assert_eq!(
            apply("Oi @ana e @444555666, tudo bem? @anabela não.", &r),
            "Oi @111222333 e @444555666, tudo bem? @anabela não."
        );
        assert_eq!(apply("Oi @+5511911111111!", &r[..1]), "Oi @111222333!");
        // nome composto, texto só com o primeiro nome (fronteira de palavra)
        let r2 = resolve(
            &g,
            &[],
            vec![q(
                "Ana Silva",
                Some("5511911111111"),
                None,
                Some("Ana Silva"),
            )],
        )
        .unwrap();
        assert_eq!(
            apply("Oi @Ana, @Anabela não.", &r2),
            "Oi @111222333, @Anabela não."
        );
        assert_eq!(apply("sem token", &r), "@111222333 @444555666 sem token");
        assert_eq!(apply("", &r[..1]), "@111222333");
    }
}
