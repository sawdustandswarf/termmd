# termmd colours for the current Omarchy theme.
#
# Omarchy expands this template into each theme as termmd.toml when the
# theme is set. termmd applies the current theme's termmd.toml before your
# own ~/.config/termmd/termmd.toml, so anything set there wins.
#
# termmd also reads the theme's colors.toml directly (the same values), so
# this file matters for themes that ship their own termmd.toml and for
# user templates in ~/.config/omarchy/themed/ that change the mapping.

[colors]
foreground = "{{ foreground }}"
background = "{{ background }}"
cursor = "{{ bright_foreground }}"
cursor_text = "{{ background }}"
selection_foreground = "{{ selection_foreground }}"
selection_background = "{{ selection_background }}"

color0 = "{{ background }}"
color1 = "{{ red }}"
color2 = "{{ green }}"
color3 = "{{ yellow }}"
color4 = "{{ blue }}"
color5 = "{{ magenta }}"
color6 = "{{ cyan }}"
color7 = "{{ foreground }}"

color8 = "{{ muted }}"
color9 = "{{ bright_red }}"
color10 = "{{ bright_green }}"
color11 = "{{ bright_yellow }}"
color12 = "{{ bright_blue }}"
color13 = "{{ bright_magenta }}"
color14 = "{{ bright_cyan }}"
color15 = "{{ bright_foreground }}"
