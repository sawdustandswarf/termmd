# termmd Markdown styles for the current Omarchy theme.
#
# Copy to ~/.config/omarchy/themed/ and Omarchy expands it into each theme
# as termmd-markdown.toml when the theme is set. termmd applies it over its
# built-in styles and under your own ~/.config/termmd/markdown.toml (see
# dist/markdown.toml for every class and key).

[h1]
fg = "{{ accent }}"
rule = "{{ accent }}"

[h2]
fg = "{{ accent }}"
rule = "{{ muted }}"

[h3]
fg = "{{ accent }}"

[hr]
rule = "{{ muted }}"

[quote]
rule = "{{ accent }}"

[table]
rule = "{{ muted }}"

[link]
fg = "{{ accent }}"
