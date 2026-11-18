#!/bin/sh
# Rich lines without Markdown: the OSC 7701 protocol directly (DESIGN.md).
o() { printf '\033]7701;%s\033\\' "$1"; }

o 'def;title;size=2 bold border=bottom pad=0.5,0.25'
o 'use;title'; printf 'Sent with OSC 7701\n'
o 'line;size=1.5 bold'; printf 'An inline 1.5x heading\n'
o 'line;align=center'; printf 'centred text\n'
o 'line;align=right'; printf 'right-aligned text\n'

# A table: a class with auto stops in one group; tabs separate the fields.
o 'def;t;stops=l:auto,r:auto,c:auto border=grid pad=0.25,0.25 group=1'
o 'use;t;head=1 bold'; printf 'Name\tSize\tKind\n'
o 'use;t'; printf 'README.md\t2,048\tfile\n'
o 'use;t'; printf 'src\t-\tdirectory\n'
o 'use;t'; printf 'a-much-longer-name.tar.gz\t10,485,760\tarchive\n'
o 'close;group=1'
printf 'back to plain text\n'

# Fixed stops: columns and percentages, no group.
o 'line;stops=l:0,c:50%,r:100%'; printf 'left\tmiddle\tright\n'
