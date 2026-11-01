#!/bin/sh
# End-to-end test of examples/todo_api.kn against a live PostgreSQL:
#   DATABASE_URL=postgres://... examples/todo_api_test.sh [path/to/kiln]
# Builds the service, starts it, and drives every route with curl.
set -eu
kiln="${1:-kiln}"
root="$(cd "$(dirname "$0")/.." && pwd)"
: "${DATABASE_URL:?set DATABASE_URL to a PostgreSQL URL}"
port="${PORT:-18080}"
tmp="$(mktemp -d)"
"$kiln" build "$root/examples/todo_api.kn" -o "$tmp/todo_api"
ADDR="127.0.0.1:$port" "$tmp/todo_api" > "$tmp/log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true; rm -rf "$tmp"' EXIT
base="http://127.0.0.1:$port"
i=0
until curl -fsS "$base/health" >/dev/null 2>&1; do
	i=$((i + 1))
	[ $i -lt 50 ] || { echo "service did not start:"; cat "$tmp/log"; exit 1; }
	sleep 0.2
done
fail() { echo "FAIL: $*"; cat "$tmp/log"; exit 1; }
# req METHOD PATH [BODY] -> prints "STATUS BODY"
req() {
	if [ $# -ge 3 ]; then
		curl -sS -o "$tmp/body" -w '%{http_code}' -X "$1" -H 'Content-Type: application/json' -d "$3" "$base$2" > "$tmp/code"
	else
		curl -sS -o "$tmp/body" -w '%{http_code}' -X "$1" "$base$2" > "$tmp/code"
	fi
	echo "$(cat "$tmp/code") $(cat "$tmp/body")"
}
expect() { # got want
	case "$1" in $2) ;; *) fail "got '$1', want '$2'" ;; esac
}
expect "$(req GET /health)" '200 {"ok":true}'
r="$(req POST /todos '{"title":"write the spec"}')"
expect "$r" '201 {"id":*,"title":"write the spec","done":false}'
id="$(echo "$r" | sed 's/.*"id":\([0-9]*\).*/\1/')"
expect "$(req GET /todos/$id)" "200 {\"id\":$id,\"title\":\"write the spec\",\"done\":false}"
expect "$(req PUT /todos/$id '{"done":true}')" "200 {\"id\":$id,\"title\":\"write the spec\",\"done\":true}"
expect "$(req GET /todos)" "200 *{\"id\":$id,\"title\":\"write the spec\",\"done\":true}*"
expect "$(req POST /todos '{"title":"  "}')" '422 {"error":"title must not be empty"}'
expect "$(req POST /todos 'not json')" '400 {"error":*}'
expect "$(req DELETE /todos/$id)" '204 '
expect "$(req GET /todos/$id)" '404 {"error":"no such todo"}'
expect "$(req DELETE /todos/$id)" '404 *'
expect "$(req GET /todos/abc)" '404 *'
expect "$(req PATCH /todos/1)" '405 *'
echo "todo_api: all checks passed"
