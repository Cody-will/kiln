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

- **Language server:** the extension starts `kiln lsp` (the `kiln` binary on your PATH, or the `kiln.server.path` setting) and gets:
  - diagnostics as you type (parse, name, type, and borrow errors);
  - hover with types, signatures, and doc comments (including the standard library);
  - go to definition, find references, and rename (within the file);
  - completion of locals, functions, types, keywords, and `.` fields and methods;
  - the document outline;
  - formatting with `kiln fmt` (Format Document, or format on save);
  - inlay type hints for `x := ...` bindings.

  **Kiln: Restart Language Server** restarts it after you rebuild `kiln`.

## Install locally

You can do either of the following.

**Copy the folder into your extensions directory (no build step):**

```sh
cd editors/vscode && npm install --omit=dev && cd -
cp -r editors/vscode ~/.vscode/extensions/kiln-lang.kiln-0.1.1
```

**Package a `.vsix` with [`vsce`](https://github.com/microsoft/vscode-vsce):**

```sh
cd editors/vscode
npx @vscode/vsce package          # produces kiln-0.1.1.vsix
code --install-extension kiln-0.1.1.vsix
```

Reload VS Code and open any `.kn` file.

## Developing

1. Open this folder in VS Code.
2. Press <kbd>F5</kbd> to launch an Extension Development Host.
3. To check scopes, use **Developer: Inspect Editor Tokens and Scopes**.

`editors/vscode/test/grammar.test.js` runs the grammar against `examples/*.kn` with `vscode-textmate` to catch regressions. Run it with `npm test`.

## Prebuilt `.vsix`

Until the extension is on the Marketplace, download
[kiln-0.1.1.vsix](https://github.com/Cody-will/kiln/releases/download/v0.1.1/kiln-0.1.1.vsix)
and run `code --install-extension kiln-0.1.1.vsix`, or use `kiln editor install vscode`
after installing Kiln.

