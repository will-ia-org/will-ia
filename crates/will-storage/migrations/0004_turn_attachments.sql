-- Anexos que acompanham o prompt de um turno (JSON: lista de will_core::Attachment).
-- Antes disso a informação vivia só na nota em texto dentro de `prompt`.
ALTER TABLE turns ADD COLUMN attachments_json TEXT;
