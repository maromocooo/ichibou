# report <file> <name> <message-or-empty>: writes a one-case JUnit report.
report() {
  if [ -z "$3" ]; then
    printf '<testsuite><testcase name="%s"/></testsuite>\n' "$2" > "$1"
  else
    printf '<testsuite><testcase name="%s"><failure>%s</failure></testcase></testsuite>\n' "$2" "$3" > "$1"
    echo "FAILED: $3"
    exit 1
  fi
}
