// Starts the Kiln language server (`kiln lsp`) for .kn files.
//
// The server binary is `kiln` on PATH, or the path in the `kiln.server.path`
// setting. It provides diagnostics, hover, go to definition, find
// references, rename, completion, document symbols, formatting (kiln fmt),
// and inlay type hints.
"use strict";

const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function start() {
	const config = vscode.workspace.getConfiguration("kiln");
	const command = config.get("server.path") || "kiln";
	const serverOptions = {
		run: { command, args: ["lsp"], transport: TransportKind.stdio },
		debug: { command, args: ["lsp"], transport: TransportKind.stdio },
	};
	const clientOptions = {
		documentSelector: [{ scheme: "file", language: "kiln" }, { scheme: "untitled", language: "kiln" }],
		outputChannelName: "Kiln Language Server",
	};
	client = new LanguageClient("kiln", "Kiln Language Server", serverOptions, clientOptions);
	return client.start().catch((err) => {
		vscode.window.showErrorMessage(
			`Could not start the Kiln language server (\`${command} lsp\`): ${err.message}. ` +
				"Install kiln or set `kiln.server.path`."
		);
	});
}

function activate(context) {
	context.subscriptions.push(
		vscode.commands.registerCommand("kiln.restartServer", async () => {
			if (client) {
				await client.stop();
			}
			await start();
		})
	);
	return start();
}

function deactivate() {
	return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
