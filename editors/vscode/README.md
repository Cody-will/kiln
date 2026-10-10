# Kiln for Visual Studio Code

This extension adds editing support for [Kiln](https://github.com/Cody-will/kiln) (`.kn` files):

- **Syntax highlighting:** a TextMate grammar (`syntaxes/kiln.tmLanguage.json`) that covers:
  - keywords, including reserved ones;
  - types, literals with suffixes, and lifetimes;
  - attributes and `///` doc comments;
  - the `:=` and `<-` operators;
  - string interpolation, where `"hi {expr}"` is highlighted as code.
- **Editing behavior:**
  - bracket matching and auto-closing;
  - comment toggling;
  - auto-indent;
  - continuing `///` doc comments on Enter.
- **Indentation:** Kiln files indent with tabs, matching `kiln fmt`.

Semantic highlighting, diagnostics, and go-to-definition will come from the Kiln language server (`kiln-ls`). It is planned but not written yet.

## Install locally

You can do either of the following.

**Copy the folder into your extensions directory (no build step):**

```sh
cp -r editors/vscode ~/.vscode/extensions/kiln-lang.kiln-0.1.0
```

**Package a `.vsix` with [`vsce`](https://github.com/microsoft/vscode-vsce):**

```sh
cd editors/vscode
npx @vscode/vsce package          # produces kiln-0.1.0.vsix
code --install-extension kiln-0.1.0.vsix
```

Reload VS Code and open any `.kn` file.

## Developing

1. Open this folder in VS Code.
2. Press <kbd>F5</kbd> to launch an Extension Development Host.
3. To check scopes, use **Developer: Inspect Editor Tokens and Scopes**.

`editors/vscode/test/grammar.test.js` runs the grammar against `examples/*.kn` with `vscode-textmate` to catch regressions. Run it with `npm test`.
