# Control flow

## `if`

Conditions have no parentheses and must be `bool`:

```text
fn describe(n: i32) -> String {
	if n < 0 {
		return "negative"
	} else if n == 0 {
		return "zero"
	}
	return "positive"
}

fn main() {
	println(describe(-4))
}
```

Hmm, `"negative"` is a `&str`, not a `String`. The checker catches that:

```text
error: mismatched types: expected `String`, found `&str`
```

Use `.to_string()` or change the return type to `&str`. Here's the fixed version, which also shows `if` as a value:

```kiln
fn describe(n: i32) -> &'static str {
	label := if n < 0 { "negative" } else if n == 0 { "zero" } else { "positive" }
	return label
}

fn main() {
	println(describe(-4))
}
```

`if` and `match` can produce a value when they appear in an expression position such as `x := ...`, `return ...`, or a function argument. Each branch's value is the last expression in its block. When used as a value, `if` needs an `else`.

## `match`

`match` compares a value against patterns, top to bottom:

```kiln
fn classify(n: u32) -> &'static str {
	return match n {
		0 => "none",
		1 | 2 | 3 => "a few",
		4..=9 => "several",
		_ => "many",
	}
}

fn main() {
	println(classify(5))
}
```

Matches must be exhaustive. For an integer you need a `_` arm; for an enum, every variant must be covered (see [Structs and enums](05-structs-enums.md)). Arms can have guards: `n if n % 2 == 0 => ...`.

## Loops

```kiln
fn main() {
	// Range: 0, 1, ..., 4 (`..=` includes the end)
	for i in 0..5 {
		println("{i}")
	}

	// Over a collection, by reference
	names := ["ada", "grace", "linus"]
	for n in &names {
		println(n)
	}

	// While
	count mut := 3
	while count > 0 {
		count -= 1
	}

	// Infinite loop; `break` can produce a value
	tries mut := 0
	result := loop {
		tries += 1
		if tries == 4 {
			break tries * 10
		}
	}
	println("{result}")
}
```

Loops can be labeled to `break` or `continue` an outer loop:

```kiln
fn main() {
	'outer: for x in 0..10 {
		for y in 0..10 {
			if x * y > 20 {
				break 'outer
			}
		}
	}
}
```

## `if let` and `while let`

When you only care about one pattern:

```kiln
fn main() {
	maybe := Some(3)
	if let Some(n) = maybe {
		println("got {n}")
	}
}
```
