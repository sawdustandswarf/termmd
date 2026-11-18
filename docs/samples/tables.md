# Tables

## Alignment

| Left          | Centre  |        Right |
| :------------ | :-----: | -----------: |
| a             |    b    |            c |
| a longer cell | centred |        12.50 |
| x             |    y    | 1,234,567.00 |

## Throughput (MiB/s, median of 10)

| Test      | termmd | foot | alacritty | kitty |
|-----------|-------:|-----:|----------:|------:|
| ascii     |     68 |   63 |        55 |    44 |
| CJK       |     54 |   50 |        59 |    37 |
| SGR       |     87 |   78 |        69 |    29 |
| truecolor |     85 |   75 |        77 |    29 |
| cursor    |     90 |   75 |        72 |    24 |
| scroll    |     52 |   50 |        72 |    37 |

Text after a table ends it.

| Not a table because no delimiter row follows |
just text

## Too wide for the window

Narrow the window: the table scrolls sideways (swipe sideways, or Shift and the wheel,
over it). With `overflow = "wrap"` under `[table]` in `markdown.toml`, its cells wrap
inside their columns instead.

| Name | What it does | Why it matters | Status |
|:---|:---|:---|---:|
| Linked documents | Shows a file in the terminal and follows changes to it from other programs | Several editors can work on one file at once without stepping on each other | 100.0 |
| `https://example.com/a/very/long/address/that/has/no/spaces/in/it` | Short | Long words break rather than squeeze the other columns | 7.25 |
