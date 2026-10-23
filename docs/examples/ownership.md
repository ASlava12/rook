# Ownership and compiler diagnostics

Borrow a value when the caller still needs to own it. The normal program and
the test harness are separate compiler targets; `--run` executes both.

```rust
fn length(value: &str) -> usize { value.len() }
fn main() {
    let owned = String::from("rook");
    assert_eq!(length(&owned), 4);
    assert_eq!(owned, "rook");
}
#[test]
fn borrowed_input_survives() {
    let input = String::from("test");
    assert_eq!(length(&input), input.len());
}
```

A move makes a later use invalid. The annotation requires the actual typed
E0382 diagnostic from this program, rather than a word in Cargo's output.

```rust,compile_fail,E0382
fn main() {
    let original = String::from("rook");
    let moved = original;
    println!("{original} {moved}");
}
```

An uncoded refusal still needs a real compiler error in the selected source.

```rust,compile_fail
compile_error!("this program is intentionally refused");
fn main() {}
```

`no_run` still builds both targets, including the body of this test. Its panic
demonstrates why opting into execution must respect the annotation.

```rust,no_run
fn main() { panic!("compile only"); }
#[test]
fn compile_only() { panic!("compile only test"); }
```

An items-only block explicitly requests an appended empty `main`.

```rust,items
fn twice(value: u32) -> u32 { value * 2 }
#[test]
fn doubles() { assert_eq!(twice(3), 6); }
```

An ignored display block is counted and reported, without creating a program.

```rust,ignore
This is an intentionally non-compiling sketch.
```
