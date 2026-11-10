. scenarios/junit.sh
status=$(curl -s -o /tmp/aperture-demo-list.json -w '%{http_code}' "$APERTURE_BASE_URL/api/notes")
[ "$status" = "200" ] || report "$1" "list notes" "GET /api/notes answered $status"
case "$(cat /tmp/aperture-demo-list.json)" in
  \[*) report "$1" "list notes" "" ;;
  *) report "$1" "list notes" "body is not a JSON array" ;;
esac
