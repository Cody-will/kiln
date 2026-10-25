// End-to-end test: launches VS Code with this extension, opens a .kn file and
// checks that it gets the kiln language and diagnostics from `kiln lsp`.
//   KILN=/path/to/kiln npm run test:e2e   (needs a display; use xvfb-run)
"use strict";
const path = require("path");
const fs = require("fs");
const os = require("os");
const { runTests } = require("@vscode/test-electron");

async function main() {
	const kiln = process.env.KILN || "kiln";
	const ws = fs.mkdtempSync(path.join(os.tmpdir(), "kiln-e2e-"));
	fs.cpSync(path.join(__dirname, "fixture"), ws, { recursive: true });
	fs.mkdirSync(path.join(ws, ".vscode"));
	fs.writeFileSync(path.join(ws, ".vscode", "settings.json"), JSON.stringify({ "kiln.server.path": kiln }));
	await runTests({
		extensionDevelopmentPath: path.resolve(__dirname, "..", ".."),
		extensionTestsPath: path.join(__dirname, "suite.js"),
		launchArgs: [ws, "--disable-extensions", "--disable-gpu", "--no-sandbox"],
	});
}

main().catch((e) => {
	console.error(e);
	process.exit(1);
});
