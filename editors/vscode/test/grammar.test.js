// Tokenizes snippets and the repository examples with the TextMate grammar and
// checks that key constructs receive the expected scopes.
const fs = require("fs");
const path = require("path");
const assert = require("assert");
const vsctm = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const wasm = fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm")).buffer;
const onigLib = oniguruma.loadWASM(wasm).then(() => ({
  createOnigScanner: (s) => new oniguruma.OnigScanner(s),
  createOnigString: (s) => new oniguruma.OnigString(s),
}));
const registry = new vsctm.Registry({
  onigLib,
  loadGrammar: async () =>
    vsctm.parseRawGrammar(
      fs.readFileSync(path.join(__dirname, "../syntaxes/kiln.tmLanguage.json"), "utf8"),
      "kiln.tmLanguage.json"
    ),
});

function tokenize(grammar, src) {
  let stack = vsctm.INITIAL;
  const out = [];
  for (const line of src.split("\n")) {
    const r = grammar.tokenizeLine(line, stack);
    for (const t of r.tokens) out.push({ text: line.slice(t.startIndex, t.endIndex), scopes: t.scopes });
    stack = r.ruleStack;
  }
  return out;
}

function scopeOf(tokens, text) {
  const t = tokens.find((t) => t.text === text);
  assert(t, `token ${JSON.stringify(text)} not found`);
  return t.scopes.join(" ");
}

const cases = [
  ["fn main() {}", "main", "entity.name.function.kiln"],
  ["fn main() {}", "fn", "keyword.declaration.function.kiln"],
  ["struct Point {}", "Point", "entity.name.type.kiln"],
  ["x mut := 5", ":=", "keyword.operator.declaration.kiln"],
  ["x mut := 5", "mut", "storage.modifier.kiln"],
  ["tx <- v", "<-", "keyword.operator.channel.kiln"],
  ["go worker(1)", "go", "keyword.control.kiln"],
  ["x := 42u8", "42u8", "constant.numeric.integer.kiln"],
  ["x := 1.5e3f64", "1.5e3f64", "constant.numeric.float.kiln"],
  ["fn f<'a>(s: &'a str) {}", "'a", "storage.modifier.lifetime.kiln"],
  ["c := 'x'", "'x'", "string.quoted.single.kiln"],
  ["s := \"a\\n\"", "\\n", "constant.character.escape.kiln"],
  ["s := \"hi {name}\"", "name", "meta.interpolation.kiln"],
  ["/// docs", "/// docs", "comment.line.documentation.kiln"],
  ["#[derive(Debug)]", "derive", "entity.name.function.attribute.kiln"],
  ["x: i64", "i64", "support.type.primitive.kiln"],
  ["return None", "None", "support.constant.core.kiln"],
  ["x.await", "await", "invalid.deprecated.reserved.kiln"],
];

registry.loadGrammar("source.kiln").then((grammar) => {
  let failures = 0;
  for (const [src, text, scope] of cases) {
    try {
      const s = scopeOf(tokenize(grammar, src), text);
      assert(s.includes(scope), `${JSON.stringify(src)}: ${JSON.stringify(text)} has scopes "${s}", expected "${scope}"`);
    } catch (e) {
      failures++;
      console.error("FAIL", e.message);
    }
  }
  // Every example must tokenize, and nothing may be left in an unterminated string/comment.
  const dir = path.join(__dirname, "../../../examples");
  for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".kn"))) {
    const toks = tokenize(grammar, fs.readFileSync(path.join(dir, f), "utf8"));
    const last = toks.filter((t) => t.text.trim()).pop();
    if (last.scopes.length !== 1) {
      failures++;
      console.error(`FAIL ${f}: ends inside ${last.scopes.join(" ")}`);
    }
  }
  console.log(`${cases.length - failures >= 0 ? cases.length : 0} scope cases, failures: ${failures}`);
  process.exit(failures ? 1 : 0);
});
