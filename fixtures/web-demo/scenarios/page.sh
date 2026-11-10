. scenarios/junit.sh
body=$(curl -s "$APERTURE_BASE_URL/")
case "$body" in
  *"<h1>Notes</h1>"*) report "$1" "page shows heading" "" ;;
  *) report "$1" "page shows heading" "home page has no Notes heading" ;;
esac
