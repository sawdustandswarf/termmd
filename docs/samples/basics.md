# termmd rich lines

Plain paragraphs stay ordinary terminal text: they wrap at the window
width and select and copy like any output.

## Headings

### Third level

#### Fourth level is bold at the normal size

---

## A table

| Terminal  | Language | Binary (KiB) | GPU    |
|:----------|:--------:|-------------:|:-------|
| termmd    | Rust     |         1900 | Vulkan |
| foot      | C        |          491 | none   |
| alacritty | Rust     |         8427 | OpenGL |
| kitty     | C/Python |           98 | OpenGL |

## Code

```
fn main() {
    println!("hello from a fenced block");
}
```

> A quoted line, with a rule down its left side.

- bullets become dots
- and stay plain text
