. scenarios/junit.sh
text="note-$$"
curl -s -X POST -H 'content-type: application/json' -d "{\"text\":\"$text\"}" "$ICHIBOU_BASE_URL/api/notes" > /dev/null
case "$(curl -s "$ICHIBOU_BASE_URL/api/notes")" in
  *"$text"*) report "$1" "note persists" "" ;;
  *) report "$1" "note persists" "created note is not in the list" ;;
esac
