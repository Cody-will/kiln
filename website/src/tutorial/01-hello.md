# Hello, world

Create a file called `hello.kn`:

```kiln
fun main() {
	println("Hello, Kiln!")
}
```

Things to notice:

- Kiln source files use the `.kn` extension.
- A program starts at `fun main()`.
- There are no semicolons. As in Go, a newline ends a statement when the line ends in something that can finish one (a name, a literal, `)`, `}`, `return`, ...).
- Blocks use braces, and the opening brace goes on the same line (`kiln fmt` style). Indent with tabs.

## Checking it

```sh
kiln check hello.kn
# hello.kn: ok
kiln run hello.kn
# Hello, Kiln!
```

<div class="status">
✅ <strong>Programs compile and run.</strong> <code>kiln run hello.kn</code> compiles the file to a native executable and runs it; <code>kiln build hello.kn</code> leaves the executable (<code>hello</code>, or <code>hello.exe</code> on Windows) in the current directory. You need a C toolchain for linking: <code>cc</code> on Linux and macOS, or the Visual Studio C++ build tools on Windows.
</div>

## Making a mistake on purpose

Change `println` to `printn` and check again:

```text
error: cannot find value `printn` in this scope
 --> hello.kn:2:2
  |
2 | 	printn("Hello, Kiln!")
  | 	^^^^^^
```

Every diagnostic shows the file, line, and column, the source line, and a caret under the problem.

## String interpolation

Strings can embed expressions with `{...}`:

```kiln
fun main() {
	name := "Kiln"
	year := 2026
	println("Hello from {name}, {year}!")
	println("Two plus two is {2 + 2}")
	println("Literal braces: {{ and }}")
}
```
