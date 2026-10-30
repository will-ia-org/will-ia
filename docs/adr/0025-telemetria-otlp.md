# ADR-0025 — Telemetria por OTLP/HTTP (Protobuf), Axiom como preset

- Status: Aceita
- Data: 2026-10-30

## Contexto

O will-ia vai rodar em várias máquinas de pessoas diferentes. Precisamos de rastreabilidade
("o que aconteceu nessa instância") e de métricas de uso (módulo mais usado, erros, quedas) sem
pedir logs por e-mail. O código tinha zero spans e os eventos não carregavam ids correlacionáveis.
O Axiom (plano grátis de 500 GB/mês, 30 dias) foi escolhido como destino inicial, mas não queremos
nos amarrar a ele.

## Decisão

1. **Protocolo padrão, não SDK de fornecedor**: o crate `will-telemetry` exporta logs e spans em
   **OTLP/HTTP com Protobuf** (`opentelemetry-proto`, só os tipos gerados; sem tonic/gRPC, sem o
   SDK do OpenTelemetry). Qualquer backend OTLP recebe sem adaptação. O Axiom é um preset
   (`endpoint` + `X-Axiom-Dataset` + Bearer); o provedor `otlp` aceita endpoint e cabeçalhos livres.
2. **Camada própria do `tracing`** em vez de `tracing-opentelemetry` + `opentelemetry-appender-tracing`:
   precisamos de (a) campos dos spans herdados pelos logs, (b) redação de segredos antes de sair do
   processo, (c) spool em disco quando não há rede, (d) reconfiguração em tempo de execução pela UI,
   (e) contadores de uso baratos. O SDK oficial não dá (a)–(d) sem contorcer; a camada tem ~300 linhas.
3. **Métricas como eventos** (`heartbeat` com deltas a cada 5 min), não OTLP Metrics: todo backend
   de logs agrega campos numéricos; evita um terceiro pipeline e dashboards presos a um fornecedor.
4. **Ids de trace reais**: cada span raiz ganha `trace_id` (16 bytes) e cada span `span_id`
   (8 bytes); logs dentro deles levam os mesmos ids → correlação nativa nos backends.
5. **Opt-in, desligada por padrão**; token no cofre de segredos; o app usa o destino do servidor
   (`GET /api/telemetry/client-config`, admin) para enviar os próprios logs.
6. **Modelo de rastreio padronizado** (nomes de spans e campos no `docs/29`), aplicado em todos os
   crates — instrumentação é parte do contrato, não detalhe de implementação.

## Consequências

- `docs/03` RNF-PRIV-01 e `docs/12` regra 8 passam de "sem telemetria" para "telemetria opt-in,
  desligada por padrão, só para o endpoint configurado".
- Dependências novas: `opentelemetry-proto` (tipos), `prost`, `gethostname`.
- Trocar de provedor = mudar `provider`/`endpoint`/`headers`; nenhum código.
- Custo em runtime: um `try_send` por evento/span e contadores atômicos; fila limitada (descarte
  contado em `dropped`), spool limitado a 20 MB.
