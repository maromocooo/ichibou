. scenarios/junit.sh
status=$(curl -s -o /tmp/ichibou-demo-list.json -w '%{http_code}' "$ICHIBOU_BASE_URL/api/notes")
[ "$status" = "200" ] || report "$1" "list notes" "GET /api/notes answered $status"
case "$(cat /tmp/ichibou-demo-list.json)" in
  \[*) report "$1" "list notes" "" ;;
  *) report "$1" "list notes" "body is not a JSON array" ;;
esac
