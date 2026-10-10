# Hello, world

Create a file called `hello.kn`:

```kiln
fn main() {
	println("Hello, Kiln!")
}
```

Things to notice:

- Kiln source files use the `.kn` extension.
- A program starts at `fn main()`.
- There are no semicolons. As in Go, a newline ends a statement when the line ends in something that can finish one (a name, a literal, `)`, `}`, `return`, ...).
- Blocks use braces, and the opening brace goes on the same line (`kiln fmt` style). Indent with tabs.

## Checking it

```sh
kiln check hello.kn
# hello.kn: ok
```

<div class="status">
🚧 <strong>Running programs is not implemented yet.</strong> When code generation lands, <code>kiln run hello.kn</code> will compile and run the file. For now, <code>kiln check</code> reports every name and type error a full build would catch before code generation.
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
fn main() {
	name := "Kiln"
	year := 2026
	println("Hello from {name}, {year}!")
	println("Two plus two is {2 + 2}")
	println("Literal braces: {{ and }}")
}
```
