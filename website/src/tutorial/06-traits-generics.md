# Traits and generics

## Traits

A trait is a set of methods a type can implement:

```kiln
trait Describe {
	fun describe(&self) string

	// A default method; implementors get it for free.
	fun shout(&self) string {
		return self.describe().to_upper()
	}
}

struct Dog {
	name string,
}

impl Describe for Dog {
	fun describe(&self) string {
		return "a dog named {self.name}"
	}
}

fun main() {
	d := Dog { name: string::from("Rex") }
	println(d.shout())
}
```

The checker verifies that an `impl` provides every required method with the same signature as the trait, and nothing extra.

## Generics

Functions, structs, enums, and impls take type parameters in `<...>`. Bounds say what a type must support:

```kiln
fun largest<T: Ord>(a T, b T) T {
	if a > b {
		return a
	}
	return b
}

struct Pair<T> {
	first T,
	second T,
}

impl<T> Pair<T> {
	fun swap(self) Pair<T> {
		return Pair { first: self.second, second: self.first }
	}
}

fun main() {
	println("{largest(3, 9)}")
	p := Pair { first: "a", second: "b" }.swap()
	println(p.first)
}
```

Type arguments are inferred from use. You can give them explicitly with the "turbofish" `::<>`: `Vec::<u8>::new()`, `chan::<string>(10)`.

Longer bounds can go in a `where` clause:

```kiln
trait Summary {
	fun summary(&self) string
}

fun print_all<T>(items &[T]) where T: Summary {
	for it in items {
		println(it.summary())
	}
}
```

If a type doesn't meet a bound, you get an error at the call site:

```text
error: the trait `Ord` is not implemented for `Point`
  = help: add `impl Ord for Point` or `#[derive(Ord)]`
```

## Deriving common traits

```kiln
@derive(Debug, Clone, Eq, Ord)
struct Version {
	major u32,
	minor u32,
}

fun main() {
	a := Version { major: 1, minor: 2 }
	b := a.clone()
	println("{a == b}")
}
```

The prelude traits are `Copy`, `Clone`, `Eq`, `Ord`, `Hash`, `Debug`, `Display`, `Default`, `Send`, `Sync`, `Error`, and `From<T>`.

## Trait objects

When you need values of different types behind one interface, use `dyn Trait` behind a reference or `Box`:

```kiln
trait Describe {
	fun describe(&self) string
}

fun show(x &dyn Describe) {
	println(x.describe())
}
```

<div class="status">
🚧 Generics are type-checked and monomorphized: each use with new types gets its own compiled copy, and <code>dyn Trait</code> calls go through vtables. Associated types (<code>type Item</code> in traits) parse but are not checked yet, and <code>impl Trait</code> in argument position is treated like <code>dyn Trait</code>. <code>#[derive]</code> is trusted by the checker; nothing is generated yet.
</div>
