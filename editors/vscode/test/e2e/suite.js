"use strict";
const vscode = require("vscode");
const assert = require("assert");
const path = require("path");

async function waitFor(f, ms) {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		const v = f();
		if (v) return v;
		await new Promise((r) => setTimeout(r, 200));
	}
	return f();
}

exports.run = async function () {
	const root = vscode.workspace.workspaceFolders[0].uri.fsPath;
	const doc = await vscode.workspace.openTextDocument(path.join(root, "main.kn"));
	await vscode.window.showTextDocument(doc);
	assert.strictEqual(doc.languageId, "kiln", "language id");
	const ext = vscode.extensions.getExtension("kiln-lang.kiln");
	assert.ok(ext, "extension installed");
	await ext.activate();
	assert.ok(ext.isActive, "extension active");
	const diags = await waitFor(() => {
		const d = vscode.languages.getDiagnostics(doc.uri);
		return d.length > 0 ? d : null;
	}, 30000);
	assert.ok(diags && diags.length > 0, "diagnostics from kiln lsp");
	console.log("diagnostic:", diags[0].range.start.line + 1, diags[0].message);
	const hover = await vscode.commands.executeCommand("vscode.executeHoverProvider", doc.uri, new vscode.Position(1, 6));
	assert.ok(hover && hover.length > 0, "hover from kiln lsp");
	console.log("hover:", hover[0].contents.map((c) => c.value || c).join(" ").slice(0, 80));
	console.log("E2E OK");
};
