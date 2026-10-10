// mdBook's bundled highlight.js has no Kiln grammar; Kiln's tokens are close
// enough to Rust's that the Rust highlighter gives good results.
(function () {
	if (typeof hljs === "undefined") {
		return;
	}
	document.querySelectorAll("code.language-kiln").forEach(function (block) {
		block.classList.remove("language-kiln");
		block.classList.add("language-rust");
		hljs.highlightBlock(block);
	});
})();
