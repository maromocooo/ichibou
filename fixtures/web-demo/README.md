# web-demo fixture

A target project with an approved-looking manifest and no product code yet.
`../web-demo-script.json` is a script for the fake agent that builds the app
step by step, so the whole Ichibou loop can be watched without spending
agent usage:

```sh
cp -R fixtures/web-demo /tmp/web-demo
ichibou --project /tmp/web-demo serve --agent fake --fake-script fixtures/web-demo-script.json
```
