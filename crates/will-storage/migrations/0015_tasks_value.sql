-- Valor da tarefa (docs/31): estimativa em centavos do que há a pagar ou
-- receber. Opcional; mínimo e máximo independentes (só um preenchido = valor único).
ALTER TABLE tasks ADD COLUMN value_min_cents INTEGER;
ALTER TABLE tasks ADD COLUMN value_max_cents INTEGER;
