#!/bin/bash
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
S=${SMOKE_DIR:-/tmp/will-ia-smoke}; mkdir -p $S/data $S/kb
PORT=47391
${WILL_IA_BIN:-$ROOT/target/debug/will-ia} serve --port $PORT --data-dir $S/data --file-secrets > $S/server.log 2>&1 &
PID=$!
trap "kill $PID 2>/dev/null; wait $PID 2>/dev/null" EXIT
for i in $(seq 1 60); do curl -sf http://127.0.0.1:$PORT/api/health >/dev/null && break; sleep 0.5; done
TOKEN=$(cat $S/data/ui-session.token)
H="Authorization: Bearer $TOKEN"
api() { curl -s -H "$H" -H "content-type: application/json" "$@"; echo; }
echo "== health"; api http://127.0.0.1:$PORT/api/health
echo "== unauthorized"; curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:$PORT/api/status
echo "== settings kb"; api -X PUT http://127.0.0.1:$PORT/api/settings -d "{\"section\":\"kb\",\"value\":{\"path\":\"$S/kb\"}}" | head -c 200; echo
echo "== settings engine (fake-claude)"; api -X PUT http://127.0.0.1:$PORT/api/settings -d "{\"section\":\"engine\",\"value\":{\"claude_bin\":\"$ROOT/target/debug/fake-claude\",\"max_live_processes\":2}}" | python3 -c "import json,sys; d=json.load(sys.stdin); print('claude_bin=',d['engine']['claude_bin'], 'tokens=',len(d['engine']['tokens']))"
echo "== scaffold"; api -X POST http://127.0.0.1:$PORT/api/kb/scaffold
echo "== add token"; api -X POST http://127.0.0.1:$PORT/api/engine/tokens -d '{"label":"fake","token":"fake-token-0123456789abcdef"}'
echo "== tokens"; api http://127.0.0.1:$PORT/api/engine/tokens
echo "== mcp-json"; api http://127.0.0.1:$PORT/api/kb/mcp-json | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d['json'])[:300])"
echo "== status"; api http://127.0.0.1:$PORT/api/status | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d['modules'])); print('mcp_url', d['mcp_url'])"
echo "== tools"; api http://127.0.0.1:$PORT/api/mcp/tools | python3 -c "import json,sys; print([ (t['name'], t['policy'], t['available']) for t in json.load(sys.stdin)])"
echo "== create thread"; TID=$(api -X POST http://127.0.0.1:$PORT/api/threads -d '{"title":"smoke"}' | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])"); echo $TID
echo "== send olá"; api -X POST http://127.0.0.1:$PORT/api/threads/$TID/messages -d '{"text":"olá"}'
sleep 2
echo "== turns"; api "http://127.0.0.1:$PORT/api/threads/$TID/turns" | python3 -c "import json,sys; [print(t['prompt'],'->',t['text']) for t in json.load(sys.stdin)]"
echo "== mid-turn"; api -X POST http://127.0.0.1:$PORT/api/threads/$TID/messages -d '{"text":"sleep 2"}'; sleep 0.5; api -X POST http://127.0.0.1:$PORT/api/threads/$TID/messages -d '{"text":"complemento"}'
sleep 4
api "http://127.0.0.1:$PORT/api/threads/$TID/turns" | python3 -c "import json,sys; [print(t['prompt'],'->',t['text']) for t in json.load(sys.stdin)]"
echo "== thread"; api http://127.0.0.1:$PORT/api/threads/$TID | python3 -c "import json,sys; d=json.load(sys.stdin); print('alive',d['alive'],'pending',d['pending'],'status',d['status'],'cost',d['total_cost_usd'],'session',d['claude_session_id'] is not None)"
echo "== restart + resume"; api -X POST http://127.0.0.1:$PORT/api/threads/$TID/restart; sleep 0.5; api -X POST http://127.0.0.1:$PORT/api/threads/$TID/messages -d '{"text":"depois do restart"}'; sleep 2
api "http://127.0.0.1:$PORT/api/threads/$TID/turns" | python3 -c "import json,sys; print([t['text'] for t in json.load(sys.stdin)][-1])"
echo "== logs"; api "http://127.0.0.1:$PORT/api/logs?limit=8" | python3 -c "import json,sys; [print(l['level'], l['module'], l['message'][:100]) for l in json.load(sys.stdin)]"
echo "== ui"; curl -s http://127.0.0.1:$PORT/ | head -c 120; echo; curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:$PORT/threads
echo "== whatsapp status"; api http://127.0.0.1:$PORT/api/whatsapp/status | head -c 200; echo
echo "== google accounts"; api http://127.0.0.1:$PORT/api/google/accounts
echo "== voice status"; api http://127.0.0.1:$PORT/api/voice/status
echo "== external mcp put"; api -X PUT http://127.0.0.1:$PORT/api/mcp/external/echo -d '{"kind":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-everything"],"env":{"SOME_TOKEN":{"$secret_value":"abc123"}}}' | head -c 300; echo
api http://127.0.0.1:$PORT/api/kb/mcp-json | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d['json']['mcpServers'].get('echo')))"
cat $S/kb/.claude/settings.local.json
echo "== server log tail"; tail -5 $S/server.log
