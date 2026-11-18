use super::*;
use crate::grid::{Color, Underline};

fn term(cols: usize, rows: usize) -> Terminal {
    Terminal::new(cols, rows, 100, Palette::default())
}

fn run(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = term(cols, rows);
    t.feed(input.as_bytes());
    t
}

fn screen(t: &Terminal) -> String {
    t.grid().screen_text()
}

fn cell(t: &Terminal, x: usize, y: usize) -> Cell {
    t.grid().row(y).cells()[x]
}

fn pos(t: &Terminal) -> (usize, usize) {
    (t.cursor().x, t.cursor().y)
}

#[test]
fn text_newlines_and_carriage_returns() {
    let t = run(10, 3, "ab\r\ncd\rX");
    assert_eq!(screen(&t), "ab\nXd\n");
    assert_eq!(pos(&t), (1, 1));
}

#[test]
fn autowrap_is_deferred_until_the_next_character() {
    let mut t = run(4, 3, "abcd");
    assert_eq!(pos(&t), (3, 0), "cursor stays on the last column");
    assert!(!t.grid().row(0).wrapped);
    t.feed(b"e");
    assert_eq!(screen(&t), "abcd\ne\n");
    assert!(t.grid().row(0).wrapped);
    // CR clears the pending wrap.
    let t = run(4, 3, "abcd\rX");
    assert_eq!(screen(&t), "Xbcd\n\n");
}

#[test]
fn no_autowrap_overwrites_the_last_column() {
    let t = run(4, 2, "\x1b[?7labcdef");
    assert_eq!(screen(&t), "abcf\n");
}

#[test]
fn scrolling_feeds_history() {
    let t = run(5, 2, "1\r\n2\r\n3\r\n4");
    assert_eq!(screen(&t), "3\n4");
    assert_eq!(t.primary().history_len(), 2);
    assert_eq!(t.primary().history_row(0).text(), "1");
}

#[test]
fn wide_characters() {
    let t = run(5, 2, "a漢b");
    assert_eq!(screen(&t), "a漢b\n");
    assert!(cell(&t, 1, 0).flags.contains(Flags::WIDE));
    assert!(cell(&t, 2, 0).flags.contains(Flags::WIDE_SPACER));
    assert_eq!(pos(&t), (4, 0));

    // No room in the last column: it is left blank and the char wraps.
    let t = run(5, 2, "abcd漢");
    assert_eq!(screen(&t), "abcd\n漢");
    assert!(t.grid().row(0).wrapped);

    // Overwriting either half blanks the other.
    let t = run(5, 1, "漢\rx");
    assert_eq!(screen(&t), "x");
    assert!(!cell(&t, 1, 0).flags.contains(Flags::WIDE_SPACER));
    let t = run(5, 1, "漢\x1b[2Gx");
    assert_eq!(screen(&t), " x");
    assert!(!cell(&t, 0, 0).flags.contains(Flags::WIDE));
}

#[test]
fn combining_marks_and_clusters_join_their_base() {
    let t = run(10, 1, "e\u{301}x");
    assert_eq!(screen(&t), "e\u{301}x");
    assert_eq!(pos(&t), (2, 0));
    assert_eq!(t.grid().row(0).extra(0).unwrap().combining, "\u{301}");

    // ZWJ emoji sequence occupies one wide cell.
    let t = run(10, 1, "👩\u{200d}💻!");
    assert_eq!(screen(&t), "👩\u{200d}💻!");
    assert_eq!(pos(&t), (3, 0));

    // A cursor move breaks the cluster: the mark has nothing to join.
    let t = run(10, 1, "e\x1b[C\u{301}");
    assert_eq!(screen(&t), "e");
    // Combining right after the last column still joins.
    let t = run(3, 2, "abe\u{301}");
    assert_eq!(screen(&t), "abe\u{301}\n");
}

#[test]
fn vs16_and_flags_widen() {
    let t = run(10, 1, "❤\u{fe0f}x");
    assert!(cell(&t, 0, 0).flags.contains(Flags::WIDE));
    assert!(cell(&t, 1, 0).flags.contains(Flags::WIDE_SPACER));
    assert_eq!(screen(&t), "❤\u{fe0f}x");
    let t = run(10, 1, "🇬🇧🇫🇷");
    assert_eq!(pos(&t), (4, 0));
    assert_eq!(screen(&t), "🇬🇧🇫🇷");
}

#[test]
fn cursor_movement_and_clamping() {
    let mut t = run(10, 5, "\x1b[3;4H");
    assert_eq!(pos(&t), (3, 2));
    t.feed(b"\x1b[2A\x1b[10D");
    assert_eq!(pos(&t), (0, 0));
    t.feed(b"\x1b[99;99H");
    assert_eq!(pos(&t), (9, 4));
    t.feed(b"\x1b[5G\x1b[2d");
    assert_eq!(pos(&t), (4, 1));
    t.feed(b"\x1b[2E");
    assert_eq!(pos(&t), (0, 3));
    t.feed(b"x\x08\x08");
    assert_eq!(pos(&t), (0, 3));
}

#[test]
fn erase() {
    let t = run(5, 3, "aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[K");
    assert_eq!(screen(&t), "aaaaa\nbb\nccccc");
    let t = run(5, 3, "aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[1K");
    assert_eq!(screen(&t), "aaaaa\n   bb\nccccc");
    let t = run(5, 3, "aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[J");
    assert_eq!(screen(&t), "aaaaa\nbb\n");
    let t = run(5, 3, "aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[1J");
    assert_eq!(screen(&t), "\n   bb\nccccc");
    let t = run(5, 3, "aaaaa\r\nbbbbb\x1b[2J");
    assert_eq!(screen(&t), "\n\n");
    let t = run(5, 1, "abcde\x1b[2G\x1b[2X");
    assert_eq!(screen(&t), "a  de");
}

#[test]
fn erase_uses_the_current_background() {
    let t = run(4, 1, "\x1b[41m\x1b[K");
    assert_eq!(cell(&t, 3, 0).bg, Color::indexed(1));
}

#[test]
fn insert_and_delete_characters() {
    let t = run(6, 1, "abcdef\x1b[2G\x1b[2@");
    assert_eq!(screen(&t), "a  bcd");
    let t = run(6, 1, "abcdef\x1b[2G\x1b[2P");
    assert_eq!(screen(&t), "adef");
    let t = run(6, 1, "abcd\x1b[2G\x1b[4hXY");
    assert_eq!(screen(&t), "aXYbcd");
    // Deleting half of a wide char blanks the rest.
    let t = run(6, 1, "a漢b\x1b[3G\x1b[P");
    assert_eq!(screen(&t), "a b");
}

#[test]
fn scroll_regions_and_line_insert_delete() {
    let mut t = run(3, 5, "1\r\n2\r\n3\r\n4\r\n5");
    t.feed(b"\x1b[2;4r");
    assert_eq!(pos(&t), (0, 0), "DECSTBM homes the cursor");
    t.feed(b"\x1b[4H\n");
    assert_eq!(screen(&t), "1\n3\n4\n\n5");
    assert_eq!(t.primary().history_len(), 0, "region scrolls never feed history");
    t.feed(b"\x1b[2H\x1bM");
    assert_eq!(screen(&t), "1\n\n3\n4\n5");
    t.feed(b"\x1b[3H\x1b[L");
    assert_eq!(screen(&t), "1\n\n\n3\n5");
    t.feed(b"\x1b[2H\x1b[2M");
    assert_eq!(screen(&t), "1\n3\n\n\n5");
    // Outside the region, IL does nothing.
    t.feed(b"\x1b[5H\x1b[L");
    assert_eq!(screen(&t), "1\n3\n\n\n5");
}

#[test]
fn delete_lines_at_the_top_never_feed_history() {
    let t = run(3, 3, "1\r\n2\r\n3\x1b[H\x1b[M");
    assert_eq!(screen(&t), "2\n3\n");
    assert_eq!(t.primary().history_len(), 0);
}

#[test]
fn origin_mode() {
    let mut t = run(5, 5, "\x1b[2;4r\x1b[?6h");
    assert_eq!(pos(&t), (0, 1));
    t.feed(b"\x1b[10;1H");
    assert_eq!(pos(&t), (0, 3), "clamped to the region");
    t.feed(b"\x1b[6n");
    assert_eq!(t.take_responses(), b"\x1b[3;1R");
}

#[test]
fn tabs() {
    let mut t = run(20, 1, "\tx");
    assert_eq!(pos(&t), (9, 0));
    t.feed(b"\x1b[3g\x1b[4G\x1bH\r\t");
    assert_eq!(pos(&t), (3, 0));
    t.feed(b"\t");
    assert_eq!(pos(&t), (19, 0), "no more stops: last column");
    t.feed(b"\x1b[Z");
    assert_eq!(pos(&t), (3, 0));
}

#[test]
fn sgr() {
    let t = run(10, 1, "\x1b[1;3;4:3;31;48;2;1;2;3;58:5:9mx\x1b[22;39mY\x1b[mz");
    let x = cell(&t, 0, 0);
    assert!(x.flags.contains(Flags::BOLD) && x.flags.contains(Flags::ITALIC));
    assert_eq!(x.flags.underline(), Underline::Curly);
    assert_eq!(x.fg, Color::indexed(1));
    assert_eq!(x.bg, Color::rgb(1, 2, 3));
    assert_eq!(t.grid().row(0).extra(0).unwrap().underline_color, Color::indexed(9));
    let y = cell(&t, 1, 0);
    assert!(!y.flags.contains(Flags::BOLD) && y.flags.contains(Flags::ITALIC));
    assert_eq!(y.fg, Color::DEFAULT);
    assert_eq!(cell(&t, 2, 0), Cell::new('z', &Pen::default()));

    let t = run(10, 1, "\x1b[38:2::10:20:30;48:2:40:50:60;95;4mx");
    let x = cell(&t, 0, 0);
    assert_eq!(x.fg, Color::indexed(13), "95 overrides the earlier 38");
    assert_eq!(x.bg, Color::rgb(40, 50, 60));
    assert_eq!(x.flags.underline(), Underline::Single);
    let t = run(10, 1, "\x1b[38;5;208mx");
    assert_eq!(cell(&t, 0, 0).fg, Color::indexed(208));
}

#[test]
fn alt_screen_1049_saves_and_restores() {
    let mut t = run(10, 3, "main\x1b[?1049h");
    assert!(t.modes().alt_screen);
    assert_eq!(screen(&t), "\n\n");
    t.feed(b"\x1b[2;2Halt");
    assert_eq!(screen(&t), "\n alt\n");
    t.feed(b"\x1b[?1049l");
    assert!(!t.modes().alt_screen);
    assert_eq!(screen(&t), "main\n\n");
    assert_eq!(pos(&t), (4, 0));
    // Re-entering starts clean.
    t.feed(b"\x1b[?1049h");
    assert_eq!(screen(&t), "\n\n");
}

#[test]
fn alt_screen_has_no_scrollback() {
    let t = run(5, 2, "\x1b[?1049h1\r\n2\r\n3\r\n4");
    assert_eq!(t.grid().history_len(), 0);
    assert_eq!(t.primary().history_len(), 0);
}

#[test]
fn save_and_restore_cursor() {
    let t = run(10, 3, "\x1b[2;3H\x1b[31m\x1b7\x1b[H\x1b[m\x1b8x");
    assert_eq!(cell(&t, 2, 1).fg, Color::indexed(1));
    assert_eq!(pos(&t), (3, 1));
}

#[test]
fn modes_and_decrqm() {
    let mut t = run(10, 3, "\x1b[?1;2004;2026;1006;1002h\x1b[?25l");
    let m = *t.modes();
    assert!(m.app_cursor && m.bracketed_paste && m.sync_output && !m.cursor_visible);
    assert_eq!(m.mouse, MouseTracking::Button);
    assert_eq!(m.mouse_encoding, MouseEncoding::Sgr);
    t.feed(b"\x1b[?2026$p\x1b[?2027$p\x1b[?9999$p\x1b[4$p");
    assert_eq!(t.take_responses(), b"\x1b[?2026;1$y\x1b[?2027;3$y\x1b[?9999;0$y\x1b[4;2$y");
    t.feed(b"\x1b[?2026l\x1b[?1002l");
    assert!(!t.modes().sync_output);
    assert_eq!(t.modes().mouse, MouseTracking::Off);
}

#[test]
fn device_reports() {
    let mut t = run(80, 24, "\x1b[c\x1b[>c\x1b[5n\x1b[3;7H\x1b[6n\x1b[18t");
    let r = String::from_utf8(t.take_responses()).unwrap();
    assert_eq!(r, "\x1b[?62;4;22;52c\x1b[>1;1;0c\x1b[0n\x1b[3;7R\x1b[8;24;80t");
    t.feed(b"\x1b[>q");
    assert!(t.take_responses().starts_with(b"\x1bP>|termmd "));
    t.set_cell_size(8, 17);
    t.feed(b"\x1b[14t\x1b[16t");
    assert_eq!(t.take_responses(), b"\x1b[4;408;640t\x1b[6;17;8t");
}

#[test]
fn color_scheme_report() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[?996n");
    assert_eq!(t.take_responses(), b"\x1b[?997;1n", "default palette is dark");
    t.feed(b"\x1b[?2031h");
    let light = Palette {
        background: Rgb::new(0xee, 0xe1, 0xc3),
        foreground: Rgb::new(0x19, 0x04, 0x03),
        ..Palette::default()
    };
    t.set_default_palette(light);
    assert_eq!(t.take_responses(), b"\x1b[?997;2n");
}

#[test]
fn cursor_style() {
    let mut t = run(10, 1, "\x1b[6 q");
    assert_eq!(t.cursor().style, CursorStyle { shape: CursorShape::Bar, blinking: false });
    t.feed(b"\x1b[3 q");
    assert_eq!(t.cursor().style, CursorStyle { shape: CursorShape::Underline, blinking: true });
    t.feed(b"\x1b[0 q");
    assert_eq!(t.cursor().style, CursorStyle::default());

    // A configured default: taken now (the style was the default), and
    // what `CSI 0 q` and RIS return to.
    let beam = CursorStyle { shape: CursorShape::Bar, blinking: true };
    t.set_default_cursor_style(beam);
    assert_eq!(t.cursor().style, beam);
    t.feed(b"\x1b[2 q");
    let block = CursorStyle { shape: CursorShape::Block, blinking: false };
    assert_eq!(t.cursor().style, block);
    // An application's choice survives a config change.
    t.set_default_cursor_style(CursorStyle::default());
    assert_eq!(t.cursor().style, block);
    t.set_default_cursor_style(beam);
    t.feed(b"\x1b[0 q");
    assert_eq!(t.cursor().style, beam);
    t.feed(b"\x1b[4 q\x1bc");
    assert_eq!(t.cursor().style, beam);
}

#[test]
fn scrollback_limit_changes() {
    let mut t = Terminal::new(10, 2, 100, Palette::default());
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.primary().history_len(), 19);
    t.set_scrollback(5);
    assert_eq!(t.primary().history_len(), 5);
    t.feed(b"more\r\nlines\r\n");
    assert_eq!(t.primary().history_len(), 5);
    t.set_scrollback(0);
    assert_eq!(t.primary().history_len(), 0);
}

#[test]
fn titles_and_title_stack() {
    let mut t = run(10, 1, "\x1b]2;hello\x07");
    assert_eq!(t.title(), "hello");
    assert_eq!(t.take_events(), [Event::TitleChanged]);
    t.feed(b"\x1b[22t\x1b]0;vim\x1b\\\x1b[23t");
    assert_eq!(t.title(), "hello");
}

#[test]
fn osc_colors_set_query_and_reset() {
    let mut t = term(10, 1);
    t.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
    assert_eq!(t.palette().indexed[1], Rgb::new(0x12, 0x34, 0x56));
    assert_eq!(t.take_responses(), b"\x1b]4;1;rgb:1212/3434/5656\x07");
    t.feed(b"\x1b]11;#102030\x1b\\\x1b]11;?\x1b\\");
    assert_eq!(t.take_responses(), b"\x1b]11;rgb:1010/2020/3030\x1b\\");
    t.feed(b"\x1b]104;1\x07\x1b]111\x07");
    assert_eq!(t.palette(), &Palette::default());
    // 10;?;? queries foreground then background.
    t.feed(b"\x1b]10;?;?\x07");
    assert_eq!(t.take_responses(), b"\x1b]10;rgb:e5e5/e5e5/e5e5\x07\x1b]11;rgb:0000/0000/0000\x07");

    // Selection colours (what omarchy-theme-osc sends), unset ones reported
    // as drawn; the chain skips pointer/Tektronix slots.
    t.feed(b"\x1b]17;?\x07\x1b]19;?\x07");
    assert_eq!(t.take_responses(), b"\x1b]17;rgb:e5e5/e5e5/e5e5\x07\x1b]19;rgb:0000/0000/0000\x07");
    t.feed(b"\x1b]17;#004584\x07\x1b]19;#eee1c3\x07\x1b]12;#190403\x07");
    assert_eq!(t.palette().selection_background, Some(Rgb::new(0x00, 0x45, 0x84)));
    assert_eq!(t.palette().selection_foreground, Some(Rgb::new(0xee, 0xe1, 0xc3)));
    t.feed(b"\x1b]16;#000000;#111111;?;#222222;?\x07\x1b]19;?\x07");
    assert_eq!(t.palette().selection_background, Some(Rgb::new(0x11, 0x11, 0x11)));
    assert_eq!(t.take_responses(), b"\x1b]19;rgb:2222/2222/2222\x07");
    t.feed(b"\x1b]117\x07\x1b]119\x07\x1b]112\x07");
    assert_eq!(t.palette(), &Palette::default());
    // The Omarchy theme script's full output applies in one go.
    t.feed(
        concat!(
            "\x1b]10;#a9b1d6\x07\x1b]11;#1a1b26\x07\x1b]12;#c0caf5\x07\x1b]17;#292e42\x07\x1b]19;#c0caf5\x07",
            "\x1b]4;0;#1a1b26\x07\x1b]4;15;#c0caf5\x07"
        )
        .as_bytes(),
    );
    assert_eq!(t.palette().background, Rgb::new(0x1a, 0x1b, 0x26));
    assert_eq!(t.palette().indexed[15], Rgb::new(0xc0, 0xca, 0xf5));
    assert!(t.take_trace().is_empty());
}

#[test]
fn hyperlinks() {
    let t = run(10, 1, "\x1b]8;id=x;https://a.example\x1b\\ab\x1b]8;;\x1b\\c");
    let row = t.grid().row(0);
    let id = row.extra(0).unwrap().hyperlink;
    assert_ne!(id, 0);
    assert_eq!(row.extra(1).unwrap().hyperlink, id);
    assert!(row.extra(2).is_none());
    let link = t.hyperlink(id).unwrap();
    assert_eq!((link.id.as_deref(), link.uri.as_str()), (Some("x"), "https://a.example"));
}

#[test]
fn clipboard_cwd_and_notifications() {
    let mut t = run(10, 1, "\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07\x1b]7;file://host/tmp/a%20b\x07");
    t.feed(b"\x1b]777;notify;Build;done\x07\x1b]9;hi\x07\x1b]9;4;1;50\x07");
    assert_eq!(
        t.take_events(),
        [
            Event::SetClipboard { primary: false, data: b"hello".to_vec() },
            Event::Notify { title: "Build".into(), body: "done".into() },
            Event::Notify { title: String::new(), body: "hi".into() },
        ]
    );
    assert!(t.take_responses().is_empty(), "clipboard reads are refused");
    assert_eq!(t.cwd(), Some("/tmp/a b"));
}

#[test]
fn dec_line_drawing() {
    let t = run(10, 1, "\x1b(0lqk\x1b(Bx");
    assert_eq!(screen(&t), "┌─┐x");
    let t = run(10, 1, "\x1b)0a\x0eq\x0fq");
    assert_eq!(screen(&t), "a─q");
}

#[test]
fn rep_decaln_and_reset() {
    let t = run(10, 1, "ab\x1b[3b");
    assert_eq!(screen(&t), "abbbb");
    let t = run(3, 2, "\x1b#8");
    assert_eq!(screen(&t), "EEE\nEEE");
    let mut t = run(10, 2, "\x1b[31mabc\x1b[?1049h\x1b]2;x\x07");
    t.take_events();
    t.feed(b"\x1bc");
    assert_eq!(screen(&t), "\n");
    assert!(!t.modes().alt_screen);
    assert_eq!(t.title(), "");
    t.feed(b"z");
    assert_eq!(cell(&t, 0, 0).fg, Color::DEFAULT);
}

#[test]
fn bell_and_hidden_controls() {
    let mut t = run(10, 1, "a\x07b\x00c");
    assert_eq!(screen(&t), "abc");
    assert_eq!(t.take_events(), [Event::Bell]);
}

#[test]
fn resize_keeps_cursor_on_content() {
    let mut t = run(10, 5, "1\r\n2\r\n3\r\n4\r\n5");
    t.resize(10, 3);
    assert_eq!(screen(&t), "3\n4\n5");
    assert_eq!(pos(&t), (1, 2));
    t.resize(4, 4);
    assert_eq!(screen(&t), "2\n3\n4\n5", "growing pulls history back");
    assert_eq!(pos(&t), (1, 3));
    t.resize(4, 5);
    assert_eq!(screen(&t), "1\n2\n3\n4\n5", "same on the same-width path");
    assert_eq!(pos(&t), (1, 4));
    t.resize(4, 4);
    t.feed(b"\x1b[?1049h\x1b[3;3H");
    t.resize(4, 2);
    assert_eq!(pos(&t), (2, 1));
    t.feed(b"\x1b[?1049l");
    assert_eq!(pos(&t), (1, 1), "primary cursor followed its content");
}

#[test]
fn resize_reflows_the_primary_screen() {
    let mut t = run(10, 3, "0123456789abc\r\n$ ");
    t.resize(5, 3);
    assert_eq!(screen(&t), "56789\nabc\n$");
    assert_eq!(pos(&t), (2, 2));
    assert_eq!(t.primary().history_len(), 1);
    t.resize(20, 3);
    assert_eq!(screen(&t), "0123456789abc\n$\n");
    assert_eq!(pos(&t), (2, 1));
    t.feed(b"ls");
    assert_eq!(screen(&t), "0123456789abc\n$ ls\n");
}

#[test]
fn resize_with_pending_wrap_keeps_the_next_char_after_the_line() {
    let mut t = run(4, 2, "abcd");
    t.resize(6, 2);
    t.feed(b"e");
    assert_eq!(screen(&t), "abcde\n");
}

/// Feeding the same bytes in any split gives the same screen.
#[test]
fn split_invariance() {
    let input =
        "\x1b[1;31mhé\u{301}llo\x1b[0m 漢字 👩\u{200d}💻 ❤\u{fe0f}\r\n\x1b]2;t\x07\x1b[2;3H\x1b[K\x1b(0lqk\x1b(B"
            .as_bytes();
    let whole = {
        let mut t = term(12, 4);
        t.feed(input);
        t
    };
    for split in 1..input.len() {
        let mut t = term(12, 4);
        t.feed(&input[..split]);
        t.feed(&input[split..]);
        assert_eq!(screen(&t), screen(&whole), "split at {split}");
        assert_eq!(pos(&t), pos(&whole));
    }
}

/// `cargo test --release -p termmd-core throughput -- --ignored --nocapture`
#[test]
#[ignore]
fn throughput() {
    let mut ascii = Vec::new();
    let mut mixed = Vec::new();
    for i in 0..200_000 {
        ascii.extend_from_slice(
            format!("line {i} the quick brown fox jumps over the lazy dog 0123456789\r\n").as_bytes(),
        );
        mixed.extend_from_slice(format!("\x1b[38;5;{}mλ→ {i} 漢字 é\x1b[0m status ok\r\n", i % 256).as_bytes());
    }
    for (name, data) in [("ascii", &ascii), ("colour+utf8", &mixed)] {
        let mut t = term(120, 40);
        let start = std::time::Instant::now();
        t.feed(data);
        let s = start.elapsed().as_secs_f64();
        println!("{name}: {:.0} MB/s", data.len() as f64 / s / 1e6);
    }
}

#[test]
fn modify_other_keys_mode() {
    let mut t = run(10, 1, "\x1b[>4;2m");
    assert_eq!(t.modes().modify_other_keys, 2);
    t.feed(b"\x1b[>4n");
    assert_eq!(t.modes().modify_other_keys, 0);
    t.feed(b"\x1b[>4;1m\x1b[>4m");
    assert_eq!(t.modes().modify_other_keys, 0);
}

/// The screen's rows as text, trailing blanks kept.
fn rows_of(t: &Terminal) -> Vec<String> {
    (0..t.rows()).map(|y| t.grid().row(y).cells().iter().map(|c| c.ch).collect()).collect()
}

#[test]
fn left_right_margins() {
    // Without DECLRMM, CSI s saves the cursor and margins don't apply.
    let mut t = run(8, 4, "\x1b[3;6s");
    assert!(!t.modes().lr_margins);
    t.feed(b"\x1b[?69h\x1b[3;6s");
    assert_eq!((t.cursor().x, t.cursor().y), (0, 0), "homes");
    // Printing wraps at the right margin back to the left one.
    t.feed(b"\x1b[1;3Habcdef");
    assert_eq!(rows_of(&t)[..2], ["  abcd  ", "  ef    "]);
    // CR goes to the left margin; BS stops there.
    t.feed(b"\r\x08\x08X");
    assert_eq!(rows_of(&t)[1], "  Xf    ");
    // Scrolling moves only the columns between the margins.
    let mut s = run(8, 4, "12345678\r\nabcdefgh\r\nABCDEFGH\r\n!@#$%^&*\x1b[?69h\x1b[3;6s\x1b[4;3H\n");
    assert_eq!(rows_of(&s), ["12cdef78", "abCDEFgh", "AB#$%^GH", "!@    &*"]);
    s.feed(b"\x1b[1;3H\x1bM");
    assert_eq!(rows_of(&s)[..2], ["12    78", "abcdefgh"]);
    // ICH/DCH stop at the right margin.
    t.feed(b"\x1b[?69l\x1b[1;1Habcdefgh\x1b[?69h\x1b[3;6s\x1b[1;4H\x1b[2@");
    assert_eq!(rows_of(&t)[0], "abc  dgh");
    t.feed(b"\x1b[1;3H\x1b[P");
    assert_eq!(rows_of(&t)[0], "ab  d gh");
    // Origin mode is relative to both margins.
    t.feed(b"\x1b[?6h\x1b[1;1H\x1b[6n");
    assert_eq!(t.take_responses(), b"\x1b[1;1R");
    assert_eq!(t.cursor().x, 2);
    t.feed(b"\x1bP$qs\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r3;6s\x1b\\");
    // Resetting DECLRMM removes them.
    t.feed(b"\x1b[?6l\x1b[?69l\x1b[1;8HZ");
    assert_eq!(rows_of(&t)[0], "ab  d gZ");
}

#[test]
fn column_insert_delete_and_side_index() {
    let mut t = run(6, 3, "abcdef\r\nghijkl\r\nmnopqr\x1b[1;2H\x1b[2'}");
    assert_eq!(rows_of(&t), ["a  bcd", "g  hij", "m  nop"]);
    t.feed(b"\x1b[1'~");
    assert_eq!(rows_of(&t), ["a bcd ", "g hij ", "m nop "]);
    // DECBI at the left edge scrolls everything right; DECFI at the right
    // edge, left.
    t.feed(b"\x1b[1;1H\x1b6");
    assert_eq!(rows_of(&t), [" a bcd", " g hij", " m nop"]);
    t.feed(b"\x1b[1;6H\x1b9");
    assert_eq!(rows_of(&t), ["a bcd ", "g hij ", "m nop "]);
    t.feed(b"\x1b9");
    assert_eq!(t.cursor().x, 5);
}

#[test]
fn protected_cells() {
    // DECSCA guards against the selective erases only.
    let mut t = run(6, 2, "a\x1b[1\"qb\x1b[0\"qc\x1b[m");
    t.feed(b"\x1b[1;1H\x1b[?K");
    assert_eq!(rows_of(&t)[0], " b    ", "SGR 0 keeps protection off/on as set");
    t.feed(b"\x1b[K");
    assert_eq!(rows_of(&t)[0], "      ");
    // SPA/EPA guard against ED, EL and ECH, not the selective ones.
    let mut t = run(6, 2, "ab\x1bVc\x1bWd\x1b[1;1H\x1b[J");
    assert_eq!(rows_of(&t)[0], "  c   ");
    t.feed(b"\x1b[4X\x1b[2K");
    assert_eq!(rows_of(&t)[0], "  c   ");
    t.feed(b"\x1b[?2J");
    assert_eq!(rows_of(&t)[0], "      ");
    t.feed(b"\x1b[1\"q\x1bP$q\"q\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r1\"q\x1b\\");
}

#[test]
fn rectangle_operations() {
    let mut t = run(6, 4, "abcdef\r\nghijkl\r\nmnopqr\r\nstuvwx");
    // DECFRA, DECERA.
    t.feed(b"\x1b[42;2;2;3;4$x");
    assert_eq!(rows_of(&t), ["abcdef", "g***kl", "m***qr", "stuvwx"]);
    t.feed(b"\x1b[1;5;2;6$z");
    assert_eq!(rows_of(&t), ["abcd  ", "g***  ", "m***qr", "stuvwx"]);
    // DECCRA, overlapping and clipped at the edge.
    t.feed(b"\x1b[3;1;4;3;1;3;5;1$v");
    assert_eq!(rows_of(&t), ["abcd  ", "g***  ", "m***m*", "stuvst"]);
    // DECSERA spares DECSCA-protected cells.
    t.feed(b"\x1b[1;1H\x1b[1\"qZ\x1b[0\"q\x1b[1;1;1;6${");
    assert_eq!(rows_of(&t)[0], "Z     ");
    // Origin mode offsets the rectangle.
    t.feed(b"\x1b[3;4r\x1b[?6h\x1b[46;1;1;1;1$x");
    assert_eq!(rows_of(&t)[2], ".***m*");
}

#[test]
fn device_reports_and_saved_modes() {
    let mut t = term(10, 4);
    t.feed(b"\x1b[?15n\x1b[?26n\x1b[?63;7n\x1b[?85n");
    assert_eq!(t.take_responses(), b"\x1b[?13n\x1b[?27;1;0;0n\x1bP7!~0000\x1b\\\x1b[?83n");
    // Unimplemented modes are permanently reset, not unknown.
    t.feed(b"\x1b[2$p\x1b[?69$p\x1b[?66h\x1b[?66$p");
    assert_eq!(t.take_responses(), b"\x1b[2;4$y\x1b[?69;2$y\x1b[?66;1$y");
    assert!(t.modes().app_keypad, "DECNKM is DECKPAM");
    // XTSAVE / XTRESTORE.
    t.feed(b"\x1b[?7;1s\x1b[?7l\x1b[?1h");
    assert!(!t.modes().autowrap && t.modes().app_cursor);
    t.feed(b"\x1b[?7;1r");
    assert!(t.modes().autowrap && !t.modes().app_cursor);
    t.feed(b"\x1bP$q*|\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r4*|\x1b\\");
}

#[test]
fn rectangle_checksum() {
    let mut t = run(10, 3, "AB\r\nC");
    t.feed(b"\x1b[1;0;1;1;2;2*y");
    assert!(!t.has_responses(), "off unless asked for");
    t.set_screen_reports(true);
    t.feed(b"\x1b[1;0;1;1;2;2*y");
    // 'A' + 'B' + 'C' + ' ' = 230, negated.
    assert_eq!(t.take_responses(), b"\x1bP1!~FF1A\x1b\\");
    // Origin mode: relative to both margins.
    t.feed(b"\x1b[?69h\x1b[2;3r\x1b[2;3s\x1b[?6h\x1b[1;1H\x1b[9;0;1;1;1;1*y");
    assert_eq!(t.take_responses(), format!("\x1bP9!~{:04X}\x1b\\", 0x10000 - ' ' as u32).as_bytes());
    // DECSTR turns DECLRMM off.
    t.feed(b"\x1b[!p");
    assert!(!t.modes().lr_margins);
    t.feed(b"\x1b[7;0;2;1;2;1*y");
    assert_eq!(t.take_responses(), format!("\x1bP7!~{:04X}\x1b\\", 0x10000 - 'C' as u32).as_bytes());
}

#[test]
fn kitty_keyboard_flags() {
    let mut t = run(10, 2, "\x1b[>1u\x1b[>11u");
    assert_eq!(t.modes().kitty_keys, 11);
    t.feed(b"\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?11u");
    // Set, add, remove.
    t.feed(b"\x1b[=4;2u");
    assert_eq!(t.modes().kitty_keys, 15);
    t.feed(b"\x1b[=1;3u");
    assert_eq!(t.modes().kitty_keys, 14);
    t.feed(b"\x1b[=2u");
    assert_eq!(t.modes().kitty_keys, 2);
    // Popping restores what was pushed under; past the bottom resets.
    t.feed(b"\x1b[<u");
    assert_eq!(t.modes().kitty_keys, 1);
    t.feed(b"\x1b[<5u");
    assert_eq!(t.modes().kitty_keys, 0);
    // Each screen has its own stack.
    t.feed(b"\x1b[>1u\x1b[?1049h");
    assert_eq!(t.modes().kitty_keys, 0);
    t.feed(b"\x1b[>31u");
    assert_eq!(t.modes().kitty_keys, 31);
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.modes().kitty_keys, 1);
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.modes().kitty_keys, 31);
    // Overflow evicts the oldest; RIS clears everything.
    for _ in 0..40 {
        t.feed(b"\x1b[>3u");
    }
    t.feed(b"\x1bc");
    assert_eq!(t.modes().kitty_keys, 0);
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.modes().kitty_keys, 0);
}

#[test]
fn decrqss_reports_settings() {
    let mut t = term(20, 10);
    t.feed(b"\x1b[1;3;4:3;31;48;5;200;58;2;1;2;3m\x1bP$qm\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r0;1;3;4:3;31;48;5;200;58;2;1;2;3m\x1b\\");
    t.feed(b"\x1b[2;8r\x1b[5 q\x1bP$qr\x1b\\\x1bP$q q\x1b\\\x1bP$qx\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r2;8r\x1b\\\x1bP1$r5 q\x1b\\\x1bP0$r\x1b\\");
    t.set_trace(true);
    t.feed(b"\x1bP$qm\x1b\\");
    assert!(t.take_trace().is_empty(), "the terminating ST is not an unknown ESC \\\\");
}

#[test]
fn trace_records_ignored_sequences() {
    let mut t = term(20, 10);
    t.feed(b"\x1b[?1h");
    assert!(t.take_trace().is_empty(), "off by default");
    t.set_trace(true);
    t.feed(b"\x1b[?12345h\x1b[>5;1z\x1b]1337;SetUserVar=a=b\x07\x1bPzq\x1b\\");
    assert_eq!(t.take_trace(), ["DECSET 12345 h", "CSI >5;1z", "OSC 1337 SetUserVar", "DCS z"]);
}

use crate::graphics::kitty::command as gfx;

fn replies(t: &mut Terminal) -> String {
    String::from_utf8(t.take_responses()).unwrap()
}

/// (line, col, span) of each placement.
fn placed(t: &Terminal) -> Vec<(u64, usize, (usize, usize))> {
    t.graphics().placements().iter().map(|p| (p.at.line, p.at.col, p.at.span)).collect()
}

#[test]
fn kitty_graphics_transmit_put_and_reply() {
    let mut t = term(20, 5);
    t.set_cell_size(10, 20);
    t.feed(gfx("i=1,f=24,s=1,v=1", &[1, 2, 3]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=1;OK\x1b\\");
    let (key, id) = t.graphics().find(1, 0).unwrap();
    assert_eq!((id, t.graphics().image(key).unwrap().rgba.as_slice()), (1, &[1, 2, 3, 255][..]));

    // Transmit and show: 25×30 px covers 3×2 cells; the cursor ends after
    // it on its last row.
    t.feed(b"\x1b[1;2H");
    t.feed(gfx("a=T,i=2,s=25,v=30,q=1", &[0; 25 * 30 * 4]).as_bytes());
    assert_eq!(replies(&mut t), "", "q=1: no OK");
    assert_eq!(placed(&t), [(0, 1, (3, 2))]);
    assert_eq!(pos(&t), (4, 1));

    // Put again with a size in cells and no cursor movement.
    t.feed(b"\x1b[3;1H");
    t.feed(gfx("a=p,i=1,p=7,c=4,r=1,C=1", &[]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=1,p=7;OK\x1b\\");
    assert_eq!(placed(&t)[1], (2, 0, (4, 1)));
    assert_eq!(pos(&t), (0, 2));

    // Errors, and quiet errors.
    t.feed(gfx("a=p,i=9", &[]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=9;ENOENT:no such image\x1b\\");
    t.feed(gfx("a=p,i=9,q=2", &[]).as_bytes());
    t.feed(gfx("i=3,f=32,s=2,v=2", &[0; 3]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=3;ENODATA:insufficient image data\x1b\\");
    // No id, no reply.
    t.feed(gfx("a=p", &[]).as_bytes());
    assert_eq!(replies(&mut t), "");
}

#[test]
fn kitty_graphics_chunks_numbers_and_queries() {
    let mut t = term(20, 5);
    let data: Vec<u8> = (0..48).collect();
    let all = gfx("i=4,s=4,v=3", &data);
    let b64 = &all[all.find(';').unwrap() + 1..all.len() - 2];
    t.feed(format!("\x1b_Gi=4,s=4,v=3,m=1;{}\x1b\\", &b64[..32]).as_bytes());
    assert!(t.graphics().find(4, 0).is_none(), "not until the last chunk");
    t.feed(format!("\x1b_Gm=0;{}\x1b\\", &b64[32..]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=4;OK\x1b\\");
    let (key, _) = t.graphics().find(4, 0).unwrap();
    assert_eq!(t.graphics().image(key).unwrap().rgba, data);

    // A number without an id gets an id assigned and both in the reply.
    t.feed(gfx("I=13,f=24,s=1,v=1", &[0; 3]).as_bytes());
    let r = replies(&mut t);
    let (_, id) = t.graphics().find(0, 13).unwrap();
    assert_eq!(r, format!("\x1b_Gi={id},I=13;OK\x1b\\"));

    // A query checks the data but stores nothing.
    t.feed(gfx("a=q,i=31,f=24,s=1,v=1", &[0; 3]).as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=31;OK\x1b\\");
    assert!(t.graphics().find(31, 0).is_none());
    t.feed(gfx("a=q,i=31,t=s,f=24,s=1,v=1", b"/x").as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=31;EBADF:transmission medium not supported\x1b\\");
}

#[test]
fn kitty_graphics_follow_scrolling() {
    let mut t = Terminal::new(10, 4, 3, Palette::default());
    t.set_cell_size(10, 20);
    t.feed(gfx("i=1,f=24,s=1,v=1", &[0; 3]).as_bytes());
    t.feed(gfx("a=p,i=1,C=1,q=2", &[]).as_bytes());
    // Scrolling into history keeps the anchor line; past the history
    // limit, the placement goes.
    t.feed(b"\x1b[4;1H\n\n");
    assert_eq!(placed(&t), [(0, 0, (1, 1))]);
    assert_eq!(t.grid().line_no(0), 2);
    t.feed(b"\n\n");
    assert!(placed(&t).is_empty(), "line 0 left history");

    // A scroll region moves rows under fixed line numbers.
    let mut t = term(10, 6);
    t.feed(gfx("i=1,f=24,s=1,v=1", &[0; 3]).as_bytes());
    t.feed(b"\x1b[2;5r\x1b[3;2H");
    t.feed(gfx("a=p,i=1,C=1,q=2", &[]).as_bytes());
    t.feed(b"\x1b[2T");
    assert_eq!(placed(&t), [(4, 1, (1, 1))]);
    t.feed(b"\x1b[1S");
    assert_eq!(placed(&t), [(3, 1, (1, 1))]);
    t.feed(b"\x1b[3S");
    assert!(placed(&t).is_empty(), "scrolled out of the region");

    // ED 2 clears what is on screen.
    t.feed(b"\x1b[r\x1b[H");
    t.feed(gfx("a=p,i=1,q=2", &[]).as_bytes());
    assert_eq!(placed(&t).len(), 1);
    t.feed(b"\x1b[2J");
    assert!(placed(&t).is_empty());
}

#[test]
fn kitty_graphics_delete() {
    let mut t = term(20, 5);
    t.set_cell_size(10, 20);
    for id in 1..=3 {
        t.feed(gfx(&format!("i={id},f=24,s=1,v=1,q=2"), &[0; 3]).as_bytes());
    }
    let put = |t: &mut Terminal, keys: &str| t.feed(gfx(&format!("a=p,C=1,q=2,{keys}"), &[]).as_bytes());
    t.feed(b"\x1b[2;3H");
    put(&mut t, "i=1");
    put(&mut t, "i=1,p=2,z=-3");
    t.feed(b"\x1b[4;6H");
    put(&mut t, "i=2");
    put(&mut t, "i=3");

    t.feed(gfx("a=d,d=i,i=1,p=2", &[]).as_bytes());
    assert_eq!(placed(&t).len(), 3, "one placement by id");
    t.feed(gfx("a=d,d=p,x=3,y=2", &[]).as_bytes());
    assert_eq!(placed(&t).len(), 2, "by cell");
    assert!(t.graphics().find(1, 0).is_some(), "lowercase keeps the image");
    t.feed(gfx("a=d,d=I,i=2", &[]).as_bytes());
    assert!(t.graphics().find(2, 0).is_none(), "uppercase frees it");
    t.feed(gfx("a=d,d=c", &[]).as_bytes());
    assert!(placed(&t).is_empty(), "at the cursor");
    t.feed(gfx("a=d,d=A", &[]).as_bytes());
    assert!(t.graphics().find(3, 0).is_some(), "named images without placements stay");
}

#[test]
fn kitty_graphics_reflow_and_reset() {
    let mut t = term(10, 4);
    t.set_cell_size(10, 20);
    t.feed(gfx("i=1,f=24,s=1,v=1,q=2", &[0; 3]).as_bytes());
    // The anchor cell is `l`, on the wrapped second row of a line.
    t.feed(b"abcdefghijklm\x1b[2;2H");
    t.feed(gfx("a=p,i=1,C=1,q=2", &[]).as_bytes());
    assert_eq!(placed(&t), [(1, 1, (1, 1))]);
    t.resize(20, 4);
    assert_eq!(placed(&t), [(0, 11, (1, 1))], "joined into one row");
    t.resize(6, 4);
    assert_eq!(placed(&t), [(1, 5, (1, 1))]);
    // Cell size changes recompute spans.
    t.feed(b"\x1b[H");
    t.feed(gfx("a=p,i=1,c=3,C=1,q=2", &[]).as_bytes());
    t.set_cell_size(5, 10);
    assert_eq!(placed(&t)[1].2, (3, 2), "15×15 px in 5×10 cells");
    t.feed(b"\x1bc");
    assert!(placed(&t).is_empty() && t.graphics().find(1, 0).is_none());
}

#[test]
fn surfaces_are_placed_like_images_and_owned_by_their_client() {
    let mut t = term(20, 5);
    t.set_cell_size(10, 20);
    let id = t.create_surface(45, 30);
    assert!(t.graphics().has_surface(id) && t.graphics().used() == 0, "no pixels held");
    t.feed(b"\x1b[2;3H");
    t.feed(gfx(&format!("a=p,i={id},q=2"), &[]).as_bytes());
    assert_eq!(placed(&t), [(1, 2, (5, 2))], "its size without c/r");
    assert_eq!(pos(&t), (7, 2));
    let p = &t.graphics().placements()[0];
    assert_eq!(t.graphics().surface(p.key), Some(id));
    t.feed(gfx(&format!("a=p,i={id},c=4,r=3,z=-1,C=1,q=2"), &[]).as_bytes());

    // Freeing deletes drop placements, not the surface; resets keep both.
    t.feed(gfx(&format!("a=d,d=I,i={id}"), &[]).as_bytes());
    assert!(placed(&t).is_empty() && t.graphics().has_surface(id));
    t.feed(gfx(&format!("a=p,i={id},q=2"), &[]).as_bytes());
    t.feed(b"\x1bc");
    assert_eq!(placed(&t), [(0, 7, (5, 2))], "back at the top, its column kept");
    // Other images never evict it; its client removes it.
    assert_ne!(t.create_surface(1, 1), id);
    t.destroy_surface(id);
    assert!(!t.graphics().has_surface(id));
    t.feed(gfx(&format!("a=p,i={id}"), &[]).as_bytes());
    assert_eq!(replies(&mut t), format!("\x1b_Gi={id};ENOENT:no such image\x1b\\"));
}

#[test]
fn surfaces_are_placed_by_the_image_rules() {
    let mut t = term(40, 10);
    t.set_cell_size(10, 20);
    let id = t.create_surface(800, 500);
    // On lines of its own at the cursor: text resumes below it.
    t.feed(b"ab");
    t.feed(format!("\x1b]7701;place;{id};at=3 w=20 h=4\x1b\\").as_bytes());
    assert_eq!(placed(&t), [(1, 3, (20, 4))]);
    assert_eq!(pos(&t), (0, 5));
    // Its own size, shrunk to fit (`max-h` is half the screen).
    t.feed(format!("\x1b]7701;place;{id}\x1b\\").as_bytes());
    assert_eq!(placed(&t)[1].2, (16, 5));

    // Fixed to a screen cell: the cursor stays, and so does it as text scrolls.
    t.feed(b"\x1b[2J\x1b[H");
    assert!(placed(&t).is_empty(), "cleared with the screen");
    // Even mid-line, as after a prompt (`termmd --embed ... &`).
    t.feed(b"$ ");
    t.feed(format!("\x1b]7701;place;{id};fixed=10,2 w=8 h=3\x1b\\").as_bytes());
    assert_eq!(pos(&t), (2, 0));
    t.feed(b"\r");
    t.feed(&b"line\r\n".repeat(100));
    let p = &t.graphics().placements()[0];
    assert_eq!(p.fixed_cell((40, 10)), Some((10, 2)), "kept after its line left history");
    // Negative coordinates count from the far edge.
    t.feed(format!("\x1b]7701;place;{id};fixed=-1,-2 w=8 h=3\x1b\\").as_bytes());
    assert_eq!(t.graphics().placements()[1].fixed_cell((40, 10)), Some((32, 6)));

    t.feed(b"\x1b]7701;place;99\x1b\\\x1b]7701;place;x\x1b\\");
    t.feed(format!("\x1b]7701;place;{id};w=lots\x1b\\").as_bytes());
    assert_eq!(replies(&mut t), "", "no answers, errors ignored");
    assert_eq!(t.graphics().placements().len(), 2);

    // Fixed placements are over the screen, not on its cells: text
    // operations leave them where they are.
    let fixed =
        |t: &Terminal| t.graphics().placements().iter().filter_map(|p| p.fixed_cell((40, 10))).collect::<Vec<_>>();
    let both: Vec<(usize, usize)> = vec![(10, 2), (32, 6)];
    t.feed(b"\x1b[2J\x1b[3J");
    assert_eq!(fixed(&t), both, "not cleared with the screen or history");
    t.feed(b"\x1b[?1049h\x1b[2J\x1b[3;8r\x1b[8H\n\n\x1b[r\x1b[?1049l");
    assert_eq!(fixed(&t), both, "nor by the alternate screen, or a scroll region scrolling");
    t.feed(gfx("a=d,d=a", &[]).as_bytes());
    t.resize(30, 8);
    assert_eq!(t.graphics().placements().len(), 2, "nor deletes of what's on the screen, or reflow");
    t.feed(b"\x1bc");
    assert_eq!(fixed(&t), both, "a surface's survive a reset (its client owns it)");
    t.feed(gfx(&format!("a=d,d=i,i={id}"), &[]).as_bytes());

    // A reset keeps text-tied ones too: back at the top, text below them.
    t.feed(b"\x1b[5Hx");
    t.feed(format!("\x1b]7701;place;{id};w=8 h=3\x1b\\").as_bytes());
    t.feed(b"\x1b[?1049h");
    t.feed(format!("\x1b]7701;place;{id};w=4 h=2\x1b\\").as_bytes());
    t.feed(format!("\x1b]7701;place;{id};fixed=0,0 w=2 h=1\x1b\\").as_bytes());
    t.feed(b"\x1bc");
    let top = t.grid().line_no(0);
    let tied: Vec<_> =
        t.graphics().placements().iter().filter(|p| p.fixed.is_none()).map(|p| (p.alt, p.at.line - top)).collect();
    assert_eq!(tied, [(false, 0), (false, 3)], "on the main screen, in order");
    assert_eq!(fixed(&t), [(0, 0)]);
    assert_eq!(pos(&t), (0, 5), "text resumes below them");
    t.feed(gfx(&format!("a=d,d=i,i={id}"), &[]).as_bytes());
    assert!(t.graphics().placements().is_empty(), "deleted by id");
}

#[test]
fn kitty_graphics_media_go_through_the_host_reader() {
    use crate::graphics::{Medium, ReadRequest, Reader};
    use std::sync::{Arc, Mutex};
    let mut t = term(20, 5);
    // Without a reader, only direct transmission.
    t.feed(gfx("i=1,t=f,f=24,s=1,v=1", b"/tmp/x").as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=1;EBADF:transmission medium not supported\x1b\\");

    let seen: Arc<Mutex<Vec<ReadRequest>>> = Arc::default();
    let log = seen.clone();
    t.set_graphics_reader(Some(Reader(Arc::new(move |r| {
        log.lock().unwrap().push(r.clone());
        match r.path.as_str() {
            "/missing" => Err("EBADF:no such file".into()),
            _ => Ok(vec![10, 20, 30]),
        }
    }))));
    t.feed(gfx("i=1,t=f,f=24,s=1,v=1,O=8,S=3", b"/img.rgb").as_bytes());
    t.feed(gfx("i=2,t=s,f=24,s=1,v=1", b"/shm-name").as_bytes());
    t.feed(gfx("i=3,t=t,f=24,s=1,v=1", b"/missing").as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\\x1b_Gi=3;EBADF:no such file\x1b\\");
    let (key, _) = t.graphics().find(1, 0).unwrap();
    assert_eq!(t.graphics().image(key).unwrap().rgba, [10, 20, 30, 255]);
    let seen = seen.lock().unwrap();
    let got: Vec<_> = seen.iter().map(|r| (r.medium, r.path.as_str(), r.offset, r.size)).collect();
    assert_eq!(
        got,
        [
            (Medium::File, "/img.rgb", 8, 3),
            (Medium::SharedMemory, "/shm-name", 0, 0),
            (Medium::TempFile, "/missing", 0, 0)
        ]
    );
    // The reader survives a reset.
    drop(seen);
    t.feed(b"\x1bc");
    t.feed(gfx("i=4,t=f,f=24,s=1,v=1", b"/img.rgb").as_bytes());
    assert_eq!(replies(&mut t), "\x1b_Gi=4;OK\x1b\\");
}

#[test]
fn sixel_images() {
    let mut t = term(20, 6);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b[c");
    assert_eq!(replies(&mut t), "\x1b[?62;4;22;52c", "DA1 offers sixel");
    // 25×45 px (1:1, transparent): 3 columns, 3 rows from (2, 1).
    t.feed(b"\x1b[2;3H\x1bP0;1q\"1;1;25;45#1;2;100;0;0!25~-!25~-!25~-!25~-!25~-!25~-!25~-!25~\x1b\\");
    let p = &t.graphics().placements()[0];
    assert_eq!((p.at.line, p.at.col, p.src, p.at.span, p.cell_bound), (1, 2, (0, 0, 25, 45), (3, 3), true));
    assert_eq!(pos(&t), (2, 3), "on the last row the image touches, same column");

    // Text over it cuts the cells it covers.
    t.feed(b"\x1b[3;4HX");
    let mut parts: Vec<_> = t.graphics().placements().iter().map(|p| (p.at.line, p.at.col, p.src)).collect();
    parts.sort();
    assert_eq!(
        parts,
        [(1, 2, (0, 0, 25, 20)), (2, 2, (0, 20, 10, 20)), (2, 4, (20, 20, 5, 20)), (3, 2, (0, 40, 25, 5))]
    );
    // Erasing the rest of the screen takes what was below.
    t.feed(b"\x1b[4;1H\x1b[J");
    assert_eq!(t.graphics().placements().len(), 3);
    t.feed(b"\x1b[2J");
    assert!(t.graphics().placements().is_empty() && t.graphics().used() == 0, "anonymous images go with them");

    // DECSDM: at the top left, cursor untouched; 8452: cursor to the right.
    let sixel = b"\x1bP;1q\"1;1;12;20#1!12~-!12~-!12~-!12~\x1b\\";
    t.feed(b"\x1b[?80h\x1b[5;5H");
    t.feed(sixel);
    let p = &t.graphics().placements()[0];
    assert_eq!((p.at.line, p.at.col, pos(&t)), (t.grid().line_no(0), 0, (4, 4)));
    t.feed(b"\x1b[?80l\x1b[?8452h\x1b[1;1H");
    t.feed(sixel);
    assert_eq!(pos(&t), (2, 0), "right of a 2×1 cell image");
    t.feed(b"\x1b[?8452$p\x1b[?80$p");
    assert_eq!(replies(&mut t), "\x1b[?8452;1$y\x1b[?80;2$y");

    // XTSMGRAPHICS.
    t.feed(b"\x1b[?1;1S\x1b[?2;1S\x1b[?2;4S\x1b[?3;1S");
    assert_eq!(replies(&mut t), "\x1b[?1;0;1024S\x1b[?2;0;200;120S\x1b[?2;0;10000;10000S\x1b[?3;1;0S");
}

#[test]
fn tall_images_scrolled_in_a_region_are_cropped() {
    // On the alternate screen rows are shuffled, not sent to history.
    let mut t = term(10, 4);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b[?1049h\x1b[2;1H");
    // 10×60 px: three rows from row 1.
    t.feed(b"\x1bP;1q\"1;1;10;60#1!10~-!10~-!10~-!10~-!10~-!10~-!10~-!10~-!10~-!10~\x1b\\");
    let first = t.grid().line_no(0);
    assert_eq!(t.graphics().placements()[0].at.line, first + 1);
    t.feed(b"\x1b[2S");
    let p = &t.graphics().placements()[0];
    assert_eq!((p.at.line, p.src), (first, (0, 20, 10, 40)), "top 20 px gone");
}

#[test]
fn iterm2_inline_images() {
    let b64 = |d: &[u8]| {
        let s = gfx("", d);
        s[s.find(';').unwrap() + 1..s.len() - 2].to_owned()
    };
    let png = b64(&crate::image::png::encode(4, 2, &[200; 24]));
    let mut t = term(20, 5);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b[2;3H");
    t.feed(format!("\x1b]1337;File=name=eA==;inline=1;width=4:{png}\x07").as_bytes());
    let p = &t.graphics().placements()[0];
    assert_eq!((p.at.line, p.at.col, p.src, p.at.span, p.cell_bound), (1, 2, (0, 0, 40, 20), (4, 1), true));
    let image = t.graphics().image(p.key).unwrap();
    assert_eq!((image.width, image.height, &image.rgba[..4]), (40, 20, &[200, 200, 200, 255][..]), "scaled");
    assert_eq!(pos(&t), (6, 1), "after it, on its last row");

    // In parts, at native size.
    let half = png.len() / 8 * 4;
    t.feed(b"\x1b]1337;MultipartFile=inline=1\x07");
    t.feed(format!("\x1b]1337;FilePart={}\x07\x1b]1337;FilePart={}\x07", &png[..half], &png[half..]).as_bytes());
    t.feed(b"\x1b]1337;FileEnd\x07");
    assert_eq!(t.graphics().placements()[1].src, (0, 0, 4, 2));
    // Downloads (no inline=1) and other formats show nothing.
    t.feed(format!("\x1b]1337;File=name=eA==:{png}\x07").as_bytes());
    t.feed(b"\x1b]1337;File=inline=1:R0lGODlh\x07");
    assert_eq!(t.graphics().placements().len(), 2);
}

fn style_of(t: &Terminal, y: usize) -> Option<crate::rich::LineStyle> {
    shown(t, y).style().cloned()
}

/// Row `y` as shown: a Markdown document's line where one is anchored.
fn shown(t: &Terminal, y: usize) -> &crate::grid::Row {
    let row = t.grid().row(y);
    t.md_line_of(row).unwrap_or(row)
}

#[test]
fn rich_lines_size_wrap_and_cut() {
    // A 2x line has half the columns: text wraps at 10 of 20, and the
    // continuation keeps the size.
    let mut t = term(20, 5);
    t.feed(b"\x1b]7701;line;size=2 bold\x1b\\0123456789abc\r\nplain");
    assert_eq!(style_of(&t, 0).map(|s| (s.size, s.bold)), Some((16, true)));
    assert_eq!(t.grid().row(0).text().trim_end(), "0123456789");
    assert_eq!(t.grid().row(1).text().trim_end(), "abc");
    assert!(style_of(&t, 1).is_some_and(|s| s.size == 16), "continuation keeps the size");
    assert!(style_of(&t, 2).is_none() && t.grid().row(2).text().starts_with("plain"));

    // Rows with stops cut instead of wrapping; HT starts fields.
    let mut t = term(12, 3);
    t.feed(b"\x1b]7701;line;stops=l:auto,r:auto\x1b\\ab\tcdefghijklmnop\r\nnext");
    let row = t.grid().row(0);
    assert_eq!(row.rich.as_ref().unwrap().fields, [2]);
    assert_eq!(row.text(), "abcdefghijkl", "cut at the edge");
    assert_eq!(t.grid().row(1).text().trim_end(), "next");
    // A plain style (or none) clears; errors are ignored.
    t.feed(b"\x1b]7701;line\x1b\\\x1b]7701;line;size=zero\x1b\\");
    assert!(style_of(&t, 1).is_none());
}

#[test]
fn rich_classes_groups_and_query() {
    let mut t = term(40, 6);
    t.feed(b"\x1b]7701;def;t;stops=l:auto,r:auto border=grid group=5\x1b\\");
    t.feed(b"\x1b]7701;use;t;head=1\x1b\\Name\tSize\r\n");
    t.feed(b"\x1b]7701;use;t\x1b\\README.md\t12\r\n");
    t.feed(b"\x1b]7701;use;t\x1b\\x\t1,234,567\r\n");
    assert!(style_of(&t, 0).is_some_and(|s| s.head && s.group == 5));
    assert!(style_of(&t, 1).is_some_and(|s| !s.head));
    assert_eq!(t.group_widths(5), Some(&[9, 9][..]), "widest of each column");
    t.feed(b"\x1b]7701;use;nope\x1b\\\x1b]7701;?\x1b\\");
    assert_eq!(replies(&mut t), "\x1b]7701;ok;v=1\x1b\\");
    // Built-in Markdown classes, and RIS forgets classes.
    t.feed(b"\x1b]7701;use;md-h2\x1b\\");
    assert!(style_of(&t, 3).is_some_and(|s| s.size == 12));
    t.feed(b"\x1bc\x1b]7701;use;t\x1b\\");
    assert!(style_of(&t, 0).is_none() && t.group_widths(5).is_none());
}

#[test]
fn markdown_styling_classes() {
    let mut t = term(40, 6);
    t.set_markdown_styling(&[
        ("md-h1".into(), vec!["size=3".into(), "fg=#102030".into()]),
        ("note".into(), vec!["bold=1".into()]),
    ]);
    t.feed(b"\x1b]7701;md\x1b\\# Big\n## Less\n\x1b]7701;md-end\x1b\\");
    let h1 = style_of(&t, 0).unwrap();
    assert_eq!(h1.size, 24, "restyled");
    assert!(h1.bold && h1.border & crate::rich::border::BOTTOM != 0, "the rest of the built-in style stays");
    assert_eq!(style_of(&t, 1).map(|s| s.size), Some(12), "built in");
    // Markdown styling classes are for programs too, and survive a reset;
    // a program's own definition wins.
    t.feed(b"\x1bc\x1b]7701;use;note\x1b\\");
    assert!(style_of(&t, 0).is_some_and(|s| s.bold));
    t.feed(b"\x1b]7701;def;note;dim\x1b\\\r\n\x1b]7701;use;note\x1b\\");
    assert!(style_of(&t, 1).is_some_and(|s| s.dim && !s.bold));
}

#[test]
fn markdown_paragraphs_join_their_lines() {
    let mut t = term(40, 12);
    let md = concat!(
        "A paragraph\n  over **three\nlines**.\n\n",
        "Hard  \nbreak\\\nhere\n",
        "- item\ncontinued\n- next\n",
        "> quoted\n> on two\n",
        "# Heading\nafter\n",
    );
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    let text: Vec<String> = (0..10).map(|y| shown(&t, y).text().trim_end().to_owned()).collect();
    assert_eq!(
        text,
        [
            "A paragraph over three lines.",
            "",
            "Hard",
            "break",
            "here",
            "• item continued",
            "• next",
            "quoted on two",
            "Heading",
            "after"
        ]
    );
    let bold = |x: usize| shown(&t, 0).cells()[x].flags.contains(Flags::BOLD);
    assert!(bold(17) && bold(23) && !bold(28), "a span across lines");
    assert!(style_of(&t, 3).is_some_and(|s| s.font_prop), "a hard break stays in the paragraph");
    // A program that pauses mid-paragraph: shown so far, then continued.
    let mut t = term(40, 4);
    t.feed(b"\x1b]7701;md\x1b\\one\n");
    t.md_idle();
    assert_eq!(shown(&t, 0).text().trim_end(), "one");
    t.feed(b"two\n\x1b]7701;md-end\x1b\\");
    assert_eq!(shown(&t, 0).text().trim_end(), "one two");
}

#[test]
fn prop_lines_wrap_between_words() {
    let mut t = term(12, 6);
    t.feed(b"\x1b]7701;md\x1b\\one two three four five\r\n\x1b]7701;md-end\x1b\\");
    let rows = |t: &Terminal, n: usize| (0..n).map(|y| shown(t, y).text().trim_end().to_owned()).collect::<Vec<_>>();
    assert_eq!(rows(&t, 3), ["one two", "three four", "five"]);
    // A word longer than the row still breaks inside it.
    let mut long = term(6, 4);
    long.feed(b"\x1b]7701;md\x1b\\a abcdefghij\r\n\x1b]7701;md-end\x1b\\");
    assert_eq!(rows(&long, 3), ["a", "abcdef", "ghij"]);
    // Resizing lays documents out again.
    t.resize(20, 6);
    assert_eq!(rows(&t, 2), ["one two three four", "five"]);
    assert_eq!(pos(&t), (0, 2), "the cursor stays after it");
    t.resize(40, 6);
    assert_eq!(rows(&t, 2), ["one two three four five", ""]);
    t.resize(12, 6);
    assert_eq!(rows(&t, 3), ["one two", "three four", "five"]);
    // Plain lines still wrap by cell.
    let plain = run(12, 3, "one two three four");
    assert_eq!(rows(&plain, 2), ["one two thre", "e four"]);
}

#[test]
fn markdown_regions() {
    let mut t = term(40, 12);
    let md = "# Title #\r\nsome text\r\n| a | bb |\r\n|:--|--:|\r\n| ccc | d |\r\n\r\n- item\r\n```\r\n# not a heading\r\n```\r\n---\r\n> quoted\r\n";
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\after").as_bytes());
    let text: Vec<String> = (0..12).map(|y| shown(&t, y).text().trim_end().to_owned()).collect();
    let size = |y: usize| style_of(&t, y).map(|s| s.size);
    assert_eq!((text[0].as_str(), size(0)), ("Title", Some(16)), "h1, closing hashes dropped");
    assert_eq!((text[1].as_str(), size(1)), ("some text", Some(8)));
    assert!(style_of(&t, 0).is_some_and(|s| s.font_prop) && style_of(&t, 1).is_some_and(|s| s.font_prop), "prop");
    // The table: header and body rows with fields, delimiter row hidden.
    assert_eq!(text[2], "abb");
    assert_eq!(shown(&t, 2).rich.as_ref().unwrap().fields, [1]);
    let head = style_of(&t, 2).unwrap();
    assert!(head.head && head.bold && head.stops.len() == 2);
    assert_eq!(head.stops[1].align, crate::rich::Align::Decimal, "right-aligned numbers on their points");
    assert_eq!(text[3], "cccd");
    assert!(style_of(&t, 4).is_none(), "a blank line ends the table");
    assert_eq!(text[5], "• item");
    assert!(style_of(&t, 5).is_some_and(|s| s.font_prop));
    assert_eq!(text[6], "# not a heading", "code is not classified");
    assert!(style_of(&t, 6).is_some_and(|s| s.band.is_some()));
    assert!(style_of(&t, 7).is_some_and(|s| s.border & crate::rich::border::MIDDLE != 0), "rule");
    assert_eq!(text[8], "quoted");
    assert_eq!(text[9], "after", "the region ended");
    assert!(!t.in_markdown());
}

#[test]
fn markdown_regions_split_across_reads_and_prompts() {
    let mut t = term(30, 6);
    let all = b"\x1b]7701;md\x1b\\## Two\r\n| x |\r\n|---|\r\n| y |\r\n\x1b]133;A\x07$ ";
    // Byte by byte: lines and the end marker arrive in pieces.
    for b in all.iter() {
        t.feed(std::slice::from_ref(b));
    }
    assert!(!t.in_markdown(), "a prompt ends the region");
    assert_eq!(shown(&t, 0).text().trim_end(), "Two");
    assert!(style_of(&t, 0).is_some_and(|s| s.size == 12));
    assert_eq!(shown(&t, 1).text().trim_end(), "x");
    assert_eq!(shown(&t, 2).text().trim_end(), "y");
    assert_eq!(shown(&t, 3).text().trim_end(), "$");

    // An idle host shows a partial line; its rest is not classified.
    let mut t = term(30, 4);
    t.feed(b"\x1b]7701;md\x1b\\# Hea");
    assert_eq!(shown(&t, 0).text().trim_end(), "");
    t.md_idle();
    assert_eq!(shown(&t, 0).text().trim_end(), "# Hea");
    t.feed(b"ding\r\n");
    assert_eq!(shown(&t, 0).text().trim_end(), "# Heading");
    assert!(style_of(&t, 0).is_none());
}

#[test]
fn rich_lines_reflow() {
    let mut t = term(20, 4);
    t.feed(b"\x1b]7701;line;size=2\x1b\\0123456789abcdef\r\n");
    t.resize(30, 4);
    assert_eq!(shown(&t, 0).text().trim_end(), "0123456789abcde");
    assert_eq!(shown(&t, 1).text().trim_end(), "f");
    assert!(style_of(&t, 0).is_some() && style_of(&t, 1).is_some());
    assert!(style_of(&t, 2).is_none());
}

#[test]
fn markdown_inline_spans() {
    let mut t = term(60, 6);
    let md = "Some **bold**, *it*, `x*y`, ~~gone~~ and [a link](https://e.org).\n| `c` | **b** \\| q |\n|---|---|\n```\n**raw**\n```\n";
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\plain").as_bytes());
    let row = |y: usize| shown(&t, y);
    assert_eq!(row(0).text().trim_end(), "Some bold, it, x*y, gone and a link.");
    let cell = |y: usize, x: usize| row(y).cells()[x];
    assert!(cell(0, 5).flags.contains(Flags::BOLD) && !cell(0, 4).flags.contains(Flags::BOLD));
    assert!(cell(0, 11).flags.contains(Flags::ITALIC));
    assert!(cell(0, 15).mono && !cell(0, 15).bg.is_default(), "code span: monospace, shaded");
    assert!(cell(0, 20).flags.contains(Flags::STRIKE));
    let link = row(0).extra(29).map(|e| e.hyperlink).unwrap_or(0);
    assert_eq!(t.hyperlink(link).map(|l| l.uri.as_str()), Some("https://e.org"));
    assert_eq!(cell(0, 29).flags.underline(), crate::grid::Underline::Single);
    // Table cells, escapes; code blocks stay verbatim; the pen is restored.
    let cells = row(1).text();
    assert!(cells.starts_with('c') && cells.trim_end().ends_with("b | q"), "{cells:?}");
    assert!(cell(1, 0).mono);
    assert_eq!(row(2).text().trim_end(), "**raw**");
    assert!(!cell(3, 0).flags.contains(Flags::BOLD) && !cell(3, 0).mono);
}

#[test]
fn markdown_lists() {
    let mut t = term(40, 8);
    let md = "1. First\n2. Second **b**\n- top\n  - nested\n    - deeper\n- [ ] todo\n- [x] done\n";
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    let text: Vec<String> = (0..7).map(|y| shown(&t, y).text().trim_end().to_owned()).collect();
    assert_eq!(text, ["1. First", "2. Second b", "• top", "  ◦ nested", "    ▪ deeper", "☐ todo", "☑ done"]);
    // Indents and markers are monospace (they line up in a prop font); the text is not.
    let row = shown(&t, 3);
    assert!(row.cells()[..4].iter().all(|c| c.mono) && !row.cells()[4].mono);
    assert!(shown(&t, 1).cells()[10].flags.contains(Flags::BOLD));
}

#[test]
fn markdown_images() {
    use crate::graphics::Reader;
    use std::sync::{Arc, Mutex};
    let mut t = term(20, 10);
    t.set_cell_size(10, 20);
    // A 40×60 image: 4 cells by 3 rows.
    let png = crate::image::png::encode(40, 60, &[128; 40 * 60 * 3]);
    let within: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let log = within.clone();
    t.set_graphics_reader(Some(Reader(Arc::new(move |r| {
        log.lock().unwrap().push(r.within.clone());
        match r.path.as_str() {
            "/docs/img/a.png" | "/abs/b.png" if r.limit == 32 << 20 => Ok(png.clone()),
            _ => Err("ENOENT".into()),
        }
    }))));
    t.set_markdown_styling(&[(IMAGE_CLASS.into(), vec!["place=center".into()])]);
    let md = "![A](img/a.png)\n![B](/abs/b.png){place=right w=2}\n![C](https://example.org/c.png)\n![D](missing.png)\n\nend\n";
    t.feed(format!("\x1b]7701;md;base=/docs/\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    // Rows are reserved at once; the pixels arrive from a loading thread.
    let key = t.graphics().placements()[0].key;
    assert!(t.graphics().is_pending(key) || t.graphics().image(key).is_some_and(|i| !i.rgba.is_empty()));
    t.wait_images();
    assert!(!t.graphics().is_pending(key) && t.graphics().image(key).unwrap().rgba.len() == 40 * 60 * 4);
    // Centred by the [image] styling; the second by its own settings.
    assert_eq!(placed(&t), [(0, 8, (4, 3)), (3, 18, (2, 2))]);
    let text: Vec<String> = (5..9).map(|y| shown(&t, y).text().trim_end().to_owned()).collect();
    assert_eq!(text, ["C", "D", "", "end"], "remote and missing images show their alt text");
    assert!(shown(&t, 5).extra(0).is_some_and(|e| e.hyperlink != 0), "as a link");
    assert!(within.lock().unwrap().iter().all(|w| w.as_deref() == Some("/docs/")), "read only beside the Markdown");
    // Limits: anywhere, and smaller files only.
    t.set_markdown_image_limits(1 << 20, true);
    t.feed(b"\x1b]7701;md;base=/docs/\x1b\\![A](img/a.png)\n\x1b]7701;md-end\x1b\\");
    t.wait_images();
    assert_eq!(within.lock().unwrap().last(), Some(&None));
    assert_eq!(placed(&t).len(), 2, "over the size limit: alt text");
}

#[test]
fn markdown_images_that_fail_to_decode_show_their_alt_text() {
    use crate::graphics::Reader;
    let mut t = term(20, 10);
    t.set_cell_size(10, 20);
    // A good header, then nothing: sized at once, fails in the background.
    let mut png = crate::image::png::encode(40, 60, &[128; 40 * 60 * 3]);
    png.truncate(40);
    t.set_graphics_reader(Some(Reader(std::sync::Arc::new(move |r| {
        Ok(png[..png.len().min(r.size as usize).max(1)].to_vec())
    }))));
    t.feed(b"\x1b]7701;md;base=/d\x1b\\![Broken](b.png)\n\x1b]7701;md-end\x1b\\after");
    assert_eq!(placed(&t).len(), 1);
    t.wait_images();
    assert!(placed(&t).is_empty());
    assert_eq!(shown(&t, 0).text().trim_end(), "Broken");
    assert_eq!(shown(&t, 3).text().trim_end(), "after", "the rows stay reserved");
}

#[test]
fn markdown_extended_syntax() {
    let mut t = term(40, 8);
    let md = "### My Heading {#custom-id}\nA note.[^1] H~2~O, X^2^ :joy:\n[^1]: The note.\n\nterm\n: definition\n";
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    let text: Vec<String> = (0..6).map(|y| shown(&t, y).text().trim_end().to_owned()).collect();
    assert_eq!(text, ["My Heading", "A note.¹ H₂O, X² 😂", "¹ The note.", "", "term", "    definition"]);
    assert!(style_of(&t, 2).is_some_and(|s| s.dim), "footnote");
    assert!(shown(&t, 4).cells()[0].flags.contains(Flags::BOLD), "the term, once its definition came");
    assert!(!shown(&t, 1).cells()[0].flags.contains(Flags::BOLD), "plain paragraphs stay as they are");
}

/// The Markdown feature set's target, end to end.
#[test]
fn markdown_cheat_sheet() {
    let mut t = term(100, 130);
    let md = include_str!("../../../docs/samples/markdown-cheat-sheet.md");
    t.feed(format!("\x1b]7701;md\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    let rows: Vec<(String, Option<u8>, &crate::grid::Row)> = (0..130)
        .map(|y| shown(&t, y))
        .filter(|r| !r.is_blank() || r.style().is_some_and(|s| s.border != 0))
        .map(|r| (r.text().trim_end().to_owned(), r.style().map(|s| s.size), r))
        .collect();
    let text: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        text,
        [
            "Markdown Cheat Sheet",
            "Thanks for visiting The Markdown Guide!",
            "This Markdown cheat sheet provides a quick overview of all the Markdown syntax elements. It can’t",
            "cover every edge case, so if you need more information about any of these elements, refer to the",
            "reference guides for basic syntax and extended syntax.",
            "Basic Syntax",
            // A space may hang past the edge.
            "These are the elements outlined in John Gruber’s original design document. All Markdown applications",
            "support these elements.",
            "Heading",
            "H1",
            "H2",
            "H3",
            "Bold",
            "bold text",
            "Italic",
            "italicized text",
            "Blockquote",
            "blockquote",
            "Ordered List",
            "1. First item",
            "2. Second item",
            "3. Third item",
            "Unordered List",
            "• First item",
            "• Second item",
            "• Third item",
            "Code",
            "code",
            "Horizontal Rule",
            "",
            "Link",
            "Markdown Guide",
            "Image",
            "alt text",
            "Extended Syntax",
            "These elements extend the basic syntax by adding additional features. Not all Markdown applications",
            "support these elements.",
            "Table",
            "SyntaxDescription",
            "HeaderTitle",
            "ParagraphText",
            "Fenced Code Block",
            "{",
            "  \"firstName\": \"John\",",
            "  \"lastName\": \"Smith\",",
            "  \"age\": 25",
            "}",
            "Footnote",
            "Here's a sentence with a footnote. ¹",
            "¹ This is the footnote.",
            "Heading ID",
            "My Great Heading",
            "Definition List",
            "term",
            "    definition",
            "Strikethrough",
            "The world is flat.",
            "Task List",
            "☑ Write the press release",
            "☐ Update the website",
            "☐ Contact the media",
            "Emoji",
            "That is so funny! 😂",
            "(See also Copying and Pasting Emoji)",
            "Highlight",
            "I need to highlight these very important words.",
            "Subscript",
            "H₂O",
            "Superscript",
            "X²",
        ]
    );
    let row = |s: &str| rows.iter().find(|r| r.0 == s).unwrap_or_else(|| panic!("{s}"));
    let sizes: Vec<Option<u8>> = ["Markdown Cheat Sheet", "Basic Syntax", "Heading", "H1", "H2", "H3", "bold text"]
        .iter()
        .map(|s| row(s).1)
        .collect();
    assert_eq!(sizes, [Some(16), Some(12), Some(10), Some(16), Some(12), Some(10), Some(8)]);
    let cell = |s: &str, x: usize| row(s).2.cells()[x];
    let link = |s: &str, x: usize| row(s).2.extra(x).is_some_and(|e| e.hyperlink != 0);
    assert!(link("Thanks for visiting The Markdown Guide!", 20) && !link("Thanks for visiting The Markdown Guide!", 0));
    assert!(link("Markdown Guide", 0) && link("alt text", 0), "links, and a remote image as one");
    assert!(cell("bold text", 0).flags.contains(Flags::BOLD));
    assert!(cell("italicized text", 0).flags.contains(Flags::ITALIC));
    assert!(row("blockquote").2.style().is_some_and(|s| s.border & crate::rich::border::LEFT != 0));
    assert!(cell("code", 0).mono, "a code span stays monospace");
    assert!(row("").2.style().is_some_and(|s| s.border & crate::rich::border::MIDDLE != 0), "the rule");
    assert!(row("SyntaxDescription").2.style().is_some_and(|s| s.head && s.group != 0));
    assert!(row("{").2.style().is_some_and(|s| s.band.is_some() && !s.font_prop), "code is shaded, monospace");
    assert!(cell("term", 0).flags.contains(Flags::BOLD));
    assert!(cell("The world is flat.", 0).flags.contains(Flags::STRIKE));
    let highlight = "I need to highlight these very important words.";
    assert!(cell(highlight, 26).bg != cell(highlight, 0).bg);
    // Still, once laid out again.
    t.resize(90, 130);
    let term = (0..130).map(|y| shown(&t, y)).find(|r| r.text().trim_end() == "term").unwrap();
    assert!(term.cells()[0].flags.contains(Flags::BOLD));
}

#[test]
fn markdown_measures_in_cells_without_fonts() {
    use crate::font::FaceStyle;
    let mut t = term(10, 2);
    let m: &dyn crate::font::Measure = t.measure.as_deref().unwrap_or(&t.cells);
    assert_eq!((m.advance('a', FaceStyle::Regular, 8, true), m.advance('漢', FaceStyle::Regular, 8, true)), (1.0, 2.0));
    t.set_cell_size(9, 18);
    let m: &dyn crate::font::Measure = t.measure.as_deref().unwrap_or(&t.cells);
    assert!(!m.has_font_prop());
    assert_eq!((m.cell().width, m.advance('a', FaceStyle::Regular, 16, true)), (9, 18.0));
}

#[test]
fn documents_are_laid_out_again_around_console_text() {
    use crate::font::{CellMetrics, FaceStyle, Measure};
    /// Prop text at half a cell a character.
    #[derive(Debug)]
    struct Half;
    impl Measure for Half {
        fn cell(&self) -> CellMetrics {
            CellMetrics { width: 2, height: 4, ..CellMetrics::default() }
        }
        fn advance(&self, c: char, s: FaceStyle, size: u8, prop: bool) -> f32 {
            let cells = crate::font::Cells(self.cell()).advance(c, s, size, prop);
            if prop { cells / 2.0 } else { cells }
        }
        fn has_font_prop(&self) -> bool {
            true
        }
    }
    let mut t = term(10, 12);
    t.feed(b"$ before\r\n\x1b]7701;md\x1b\\one two three four\n\nfive six seven\n\x1b]7701;md-end\x1b\\$ after");
    let rows = |t: &Terminal| (0..8).map(|y| shown(t, y).text().trim_end().to_owned()).collect::<Vec<_>>();
    assert_eq!(rows(&t), ["$ before", "one two", "three four", "", "five six", "seven", "$ after", ""]);
    // Fonts in which prop text is half as wide: fewer rows, the console
    // text after moving up with the cursor.
    t.set_measure(Some(Box::new(Half)));
    assert_eq!(rows(&t), ["$ before", "one two three four", "", "five six seven", "$ after", "", "", ""]);
    assert_eq!(pos(&t), (7, 4));
    t.set_measure(None);
    assert_eq!(rows(&t), ["$ before", "one two", "three four", "", "five six", "seven", "$ after", ""]);
    assert_eq!(pos(&t), (7, 6));
}

/// A file shown as a linked document: its id.
fn link(t: &mut Terminal, text: &str) -> u32 {
    let doc = t.link_file("tok".into(), text.as_bytes(), "/d".into());
    t.feed(b"\x1b]7701;link;tok\x1b\\");
    doc
}

/// The text the screen's rows show.
fn shown_text(t: &Terminal) -> Vec<String> {
    (0..t.rows()).map(|y| shown(t, y).text().trim_end().to_owned()).collect()
}

#[test]
fn linked_documents_show_at_the_output_they_belong_to() {
    let mut t = term(40, 8);
    t.feed(b"before\r\n");
    let doc = t.link_file("tok".into(), b"# Title\n\nbody\n", "/d".into());
    assert!(t.has_linked(doc));
    assert_eq!(shown(&t, 1).text().trim_end(), "", "nothing until its place in the output");
    t.feed(b"more\r\n");
    t.feed(b"\x1b]7701;link;wrong\x1b\\");
    assert_eq!(shown(&t, 2).text().trim_end(), "", "a token must match");
    t.feed(b"\x1b]7701;link;tok\x1b\\after");
    assert_eq!(&shown_text(&t)[..6], ["before", "more", "Title", "", "body", "after"]);
    // A token works once.
    t.feed(b"\r\n\x1b]7701;link;tok\x1b\\");
    assert_eq!(shown_text(&t)[6], "");
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"# Title\n\nbody\n");
}

#[test]
fn linked_documents_reload_in_place() {
    let mut t = term(40, 10);
    t.feed(b"before\r\n");
    let doc = link(&mut t, "one\n");
    t.feed(b"after\r\n");
    assert_eq!(&shown_text(&t)[..3], ["before", "one", "after"]);
    assert!(!t.md_reload(doc, b"one\n"), "the same text");
    assert!(t.md_reload(doc, b"one\n\ntwo\n\nthree\n"));
    assert_eq!(&shown_text(&t)[..6], ["before", "one", "", "two", "", "three"]);
    assert_eq!(shown_text(&t)[6], "after", "the console text moved down");
    assert_eq!(pos(&t), (0, 7), "and the cursor with it");
    assert!(t.md_reload(doc, b"x\n"));
    assert_eq!(&shown_text(&t)[..3], ["before", "x", "after"]);
    assert_eq!(pos(&t), (0, 3));
    // An empty file keeps its place, for when it has text.
    assert!(t.md_reload(doc, b""));
    assert_eq!(&shown_text(&t)[..3], ["before", "", "after"]);
    assert!(t.md_reload(doc, b"back\n"));
    assert_eq!(&shown_text(&t)[..3], ["before", "back", "after"]);
}

#[test]
fn nothing_in_a_linked_file_runs() {
    let mut t = term(40, 6);
    link(&mut t, "\x1b[31mred\n\x1b]7701;md-end\x1b\\x\n\x1bc\n");
    t.feed(b"after");
    let text = shown_text(&t).join("\n");
    assert!(text.contains("␛[31mred"), "{text}");
    assert!(text.contains("after"), "the reset did nothing: {text}");
    assert_eq!(cell(&t, 0, pos(&t).1).fg, crate::grid::Color::default());
}

#[test]
fn a_reset_discards_linked_documents() {
    let mut t = term(40, 6);
    let doc = link(&mut t, "one\n");
    let waiting = t.link_file("later".into(), b"two\n", "/d".into());
    t.feed(b"\x1bc");
    assert!(!t.has_linked(doc) && !t.has_linked(waiting));
    t.feed(b"\x1b]7701;link;later\x1b\\");
    assert_eq!(shown_text(&t)[0], "");
    assert!(!t.md_reload(doc, b"new\n"));
}

#[test]
fn linked_images_keep_their_rows_through_reloads() {
    let mut t = term(40, 12);
    let png = crate::image::png::encode(40, 60, &[128; 40 * 60 * 3]);
    t.set_graphics_reader(Some(crate::graphics::Reader(std::sync::Arc::new(move |_| Ok(png.clone())))));
    t.feed(b"before\r\n");
    let doc = link(&mut t, "![A](a.png)\ntext\n");
    t.feed(b"after");
    t.wait_images();
    let placed: Vec<_> = t.graphics().placements().iter().map(|p| p.at.line).collect();
    assert_eq!(placed, [1]);
    let span = t.graphics().placements()[0].at.span.1;
    assert_eq!(shown_text(&t)[1 + span], "text");
    assert_eq!(shown_text(&t)[2 + span], "after");
    // A line above it moves it, and the old placement goes.
    assert!(t.md_reload(doc, b"new\n![A](a.png)\ntext\n"));
    t.wait_images();
    let placed: Vec<_> = t.graphics().placements().iter().map(|p| p.at.line).collect();
    assert_eq!(placed, [2]);
    assert_eq!(shown_text(&t)[1], "new");
    assert_eq!(shown_text(&t)[2 + span], "text");
    assert!(t.md_reload(doc, b"gone\n"));
    assert!(t.graphics().placements().is_empty());
    assert_eq!(&shown_text(&t)[..3], ["before", "gone", "after"]);
}

fn at(line: usize, byte: usize) -> crate::md::source::SourcePos {
    crate::md::source::SourcePos { line, byte }
}

#[test]
fn editing_a_linked_document_moves_its_rows() {
    let mut t = term(40, 10);
    t.feed(b"before\r\n");
    let doc = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after\r\n");
    assert_eq!(&shown_text(&t)[..5], ["before", "one", "", "two", "after"]);
    // The same number of lines.
    assert_eq!(t.md_edit(doc, at(0, 3)..at(0, 3), "!"), Some(at(0, 4)));
    assert_eq!(&shown_text(&t)[..5], ["before", "one!", "", "two", "after"]);
    assert_eq!(pos(&t), (0, 5));
    // More lines: console text and the cursor move down.
    assert_eq!(t.md_edit(doc, at(2, 3)..at(2, 3), "\n\nthree"), Some(at(4, 5)));
    assert_eq!(&shown_text(&t)[..7], ["before", "one!", "", "two", "", "three", "after"]);
    assert_eq!(pos(&t), (0, 7));
    // Fewer.
    let source = String::from_utf8(t.linked_source(doc).unwrap().to_bytes()).unwrap();
    assert_eq!(source, "one!\n\ntwo\n\nthree\n");
    t.md_edit(doc, at(0, 4)..at(2, 3), "").unwrap();
    assert_eq!(&shown_text(&t)[..5], ["before", "one!", "", "three", "after"]);
    assert_eq!(pos(&t), (0, 5));
}

#[test]
fn undoing_edits_and_when_a_document_is_dirty() {
    let mut t = term(40, 10);
    let doc = link(&mut t, "one\n");
    let text = |t: &Terminal| String::from_utf8(t.linked_source(doc).unwrap().to_bytes()).unwrap();
    assert!(!t.md_dirty(doc));
    // Typing is one edit.
    for (i, c) in "abc".chars().enumerate() {
        t.md_edit(doc, at(0, 3 + i)..at(0, 3 + i), &c.to_string()).unwrap();
    }
    assert_eq!(text(&t), "oneabc\n");
    assert!(t.md_dirty(doc));
    t.md_edit(doc, at(0, 0)..at(0, 1), "O").unwrap();
    assert_eq!(text(&t), "Oneabc\n");
    t.md_mark_saved(doc);
    assert!(!t.md_dirty(doc));

    assert_eq!(t.md_undo(doc), Some(at(0, 1)));
    assert_eq!(text(&t), "oneabc\n");
    assert!(t.md_dirty(doc), "it differs from what was saved");
    assert_eq!(t.md_redo(doc), Some(at(0, 1)));
    assert_eq!(text(&t), "Oneabc\n");
    assert!(!t.md_dirty(doc), "undoing back to the saved text is clean");
    t.md_undo(doc).unwrap();
    assert_eq!(t.md_undo(doc), Some(at(0, 3)));
    assert_eq!(text(&t), "one\n");
    assert_eq!(t.md_undo(doc), None);
    assert_eq!(shown_text(&t)[0], "one");
    // A new edit drops what was undone.
    t.md_redo(doc).unwrap();
    assert_eq!(text(&t), "oneabc\n");
    t.md_edit(doc, at(0, 0)..at(0, 0), "x\ny").unwrap();
    assert_eq!(t.md_redo(doc), None);
    assert_eq!(shown_text(&t)[0], "x yoneabc", "one paragraph");
    assert!(t.md_dirty(doc));
    t.md_undo(doc).unwrap();
    assert_eq!(shown_text(&t)[0], "oneabc");
    // A file that isn't UTF-8 can be shown but not changed.
    let mut t = term(40, 10);
    let doc = link_bytes(&mut t, b"a\xff\n");
    assert_eq!(t.md_edit(doc, at(0, 0)..at(0, 0), "x"), None);
}

fn link_bytes(t: &mut Terminal, bytes: &[u8]) -> u32 {
    let doc = t.link_file("tok".into(), bytes, "/d".into());
    t.feed(b"\x1b]7701;link;tok\x1b\\");
    doc
}

#[test]
fn edits_give_what_loading_the_text_does() {
    const PIECES: [&str; 12] =
        ["x", "\n", "\n\n", "# ", "```\n", "---\n", "| a | b |\n", "|---|--:|\n", ": def\n", "> ", "- ", ""];
    let mut seed = 7u64;
    let mut below = move |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n.max(1)
    };
    let mut t = term(40, 300);
    t.feed(b"before\r\n");
    let doc = link(&mut t, "# Title\n\nsome **text**\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- item\n\nend\n");
    t.feed(b"after");
    for step in 0..300 {
        let source = t.linked_source(doc).unwrap().clone();
        let lines = source.lines();
        let line = below(lines.len().max(1));
        let len = lines.get(line).map_or(0, String::len);
        let start = at(line, below(len + 1));
        let end = if below(4) == 0 { at(line, start.byte + below(len + 1 - start.byte)) } else { start };
        t.md_edit(doc, start..end, PIECES[below(PIECES.len())]).unwrap();

        let mut fresh = term(40, 300);
        fresh.feed(b"before\r\n");
        let text = t.linked_source(doc).unwrap().to_bytes();
        link_bytes(&mut fresh, &text);
        fresh.feed(b"after");
        assert_eq!(shown_text(&t), shown_text(&fresh), "step {step}: {:?}", String::from_utf8_lossy(&text));
        assert_eq!(pos(&t), pos(&fresh), "step {step}");
    }
}

/// The cell of row `y` showing the edit cursor.
fn inverse_at(t: &Terminal, y: usize) -> Option<usize> {
    shown(t, y).cells().iter().position(|c| c.flags.contains(crate::grid::Flags::INVERSE))
}

#[test]
fn the_active_block_shows_its_source() {
    let mut t = term(40, 12);
    t.feed(b"before\r\n");
    let doc = link(&mut t, "# Title\n\nsome **bold** text\n\n- item\n\n| a |\n|---|\n| 1 |\n");
    t.feed(b"after");
    let rendered = shown_text(&t);
    assert_eq!(rendered[1], "Title");
    assert_eq!(rendered[3], "some bold text");
    // Heading: it still reads as one, with its source below, marker dimmed.
    let big = shown(&t, 1).style().cloned();
    assert!(t.md_session_start(doc, at(0, 0)));
    assert_eq!(&shown_text(&t)[1..3], ["Title", "# Title"]);
    assert_eq!(shown(&t, 1).style().cloned(), big, "a heading stays a heading");
    assert!(shown(&t, 2).cells()[0].flags.contains(crate::grid::Flags::DIM));
    assert!(!shown(&t, 2).cells()[2].flags.contains(crate::grid::Flags::DIM));
    assert_eq!(inverse_at(&t, 1), None);
    assert_eq!(inverse_at(&t, 2), Some(0));
    assert_eq!(t.md_session_line(doc), Some(2));
    assert_eq!(shown_text(&t)[4], "some bold text", "others stay rendered");
    // Moving to another block swaps which shows its source.
    t.md_session_move(doc, crate::term::EditMove::Lines(2));
    assert_eq!(&shown_text(&t)[1..5], ["Title", "", "some bold text", "some **bold** text"]);
    assert_eq!(inverse_at(&t, 4), Some(0));
    t.md_session_move(doc, crate::term::EditMove::End);
    assert_eq!(inverse_at(&t, 4), Some(18), "past the last character");
    // Ending goes back to how it read.
    t.md_session_end(doc);
    assert_eq!(shown_text(&t), rendered);
    assert_eq!(inverse_at(&t, 3), None);
}

#[test]
fn typing_in_an_edit_session() {
    use crate::term::EditMove::*;
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after");
    let text = |t: &Terminal| String::from_utf8(t.linked_source(doc).unwrap().to_bytes()).unwrap();
    t.md_session_start(doc, at(0, 0));
    t.md_session_type(doc, "# ");
    assert_eq!(text(&t), "# one\n\ntwo\n");
    assert_eq!(&shown_text(&t)[..2], ["one", "# one"], "reading as typed");
    assert_eq!(inverse_at(&t, 1), Some(2));
    t.md_session_move(doc, End);
    t.md_session_type(doc, "é\n\nnew");
    assert_eq!(text(&t), "# oneé\n\nnew\n\ntwo\n");
    assert_eq!(t.md_session_cursor(doc), Some(at(2, 3)));
    assert_eq!(&shown_text(&t)[..6], ["oneé", "", "new", "new", "", "two"]);
    assert_eq!(shown_text(&t)[6], "after");
    t.md_session_delete(doc, false);
    assert_eq!(text(&t), "# oneé\n\nne\n\ntwo\n");
    // Across characters and line ends.
    t.md_session_move(doc, Up);
    t.md_session_move(doc, Up);
    t.md_session_move(doc, End);
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 7)));
    t.md_session_move(doc, Left);
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 5)), "over é's two bytes");
    t.md_session_delete(doc, true);
    assert_eq!(text(&t), "# one\n\nne\n\ntwo\n");
    t.md_session_delete(doc, true);
    assert_eq!(text(&t), "# one\nne\n\ntwo\n", "joining lines");
    t.md_session_undo(doc, false);
    assert_eq!(text(&t), "# one\n\nne\n\ntwo\n");
    t.md_session_undo(doc, true);
    assert_eq!(text(&t), "# one\nne\n\ntwo\n");
    t.md_session_end(doc);
    assert_eq!(&shown_text(&t)[..2], ["one", "ne"]);
    // An empty file can be typed into.
    let mut t = term(40, 6);
    let doc = link(&mut t, "");
    t.feed(b"after");
    assert!(t.md_session_start(doc, at(0, 0)));
    assert_eq!(inverse_at(&t, 1), Some(0));
    t.md_session_type(doc, "hi");
    t.md_session_end(doc);
    assert_eq!(&shown_text(&t)[..2], ["hi", "after"]);
}

#[test]
fn a_long_line_wraps_in_the_source_view() {
    use crate::term::EditMove::*;
    let mut t = term(10, 12);
    let doc = link(&mut t, "aaaa bbbb cccc dddd\nend\n");
    t.feed(b"x");
    t.md_session_start(doc, at(0, 0));
    // Inside the box, there's a cell less on each side.
    assert_eq!(&shown_text(&t)[..8], ["aaaa bbbb", "cccc dddd", "end", "aaaa", "bbbb", "cccc", "dddd", "end"]);
    assert_eq!(inverse_at(&t, 3), Some(0));
    t.md_session_move(doc, Down);
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 5)), "a row down, the same source line");
    assert_eq!(inverse_at(&t, 4), Some(0));
    assert_eq!(t.md_session_line(doc), Some(4));
    t.md_session_move(doc, Up);
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 0)));
    // A resize keeps the session.
    t.resize(30, 12);
    assert_eq!(&shown_text(&t)[..3], ["aaaa bbbb cccc dddd end", "aaaa bbbb cccc dddd", "end"]);
    assert_eq!(inverse_at(&t, 1), Some(0));
    t.md_session_end(doc);
    assert_eq!(&shown_text(&t)[..2], ["aaaa bbbb cccc dddd end", "x"]);
}

#[test]
fn the_source_view_is_boxed_code() {
    use crate::rich::border;
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\ntwo\nthree\n\n# Title\n");
    t.feed(b"x");
    let plain = shown(&t, 0).style().cloned();
    t.md_session_start(doc, at(0, 0));
    assert_eq!(&shown_text(&t)[..4], ["one two three", "one", "two", "three"]);
    assert_eq!(shown(&t, 0).style().cloned(), plain, "the rendered view keeps its look");
    let (first, middle, last) =
        (shown(&t, 1).style().cloned(), shown(&t, 2).style().cloned(), shown(&t, 3).style().cloned());
    let (first, middle, last) = (first.unwrap(), middle.unwrap(), last.unwrap());
    assert_eq!(first.border, border::SIDES | border::TOP);
    assert_eq!(middle.border, border::SIDES);
    assert_eq!(last.border, border::SIDES | border::BOTTOM);
    assert!(!first.font_prop && !middle.font_prop && !last.font_prop, "monospace, as code is");
    assert!(first.band.is_some(), "with code's background");
    // A single row is closed at both ends, even for a heading.
    t.md_session_move(doc, crate::term::EditMove::Lines(4));
    let title = (0..12).find(|&y| shown_text(&t)[y] == "# Title").unwrap();
    let boxed = shown(&t, title).style().cloned().unwrap();
    assert_eq!(boxed.border, border::SIDES | border::TOP | border::BOTTOM);
    assert_eq!(boxed.size, crate::rich::UNIT, "code's size, not the heading's");
    assert_eq!(shown_text(&t)[title - 1], "Title");
    t.md_session_end(doc);
}

#[test]
fn an_image_stays_above_its_source() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "before\n\n![alt](/nowhere.png)\n\nafter\n");
    t.feed(b"x");
    let rendered = shown_text(&t);
    t.md_session_start(doc, at(2, 0));
    assert!(shown_text(&t).iter().any(|l| l == "![alt](/nowhere.png)"));
    t.md_session_end(doc);
    assert_eq!(shown_text(&t), rendered);
}

#[test]
fn another_program_changes_a_file_under_a_session() {
    let mut t = term(40, 8);
    let doc = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after");
    t.md_session_start(doc, at(2, 3));
    assert!(t.md_reload(doc, b"x\n"));
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 1)));
    assert_eq!(&shown_text(&t)[..2], ["x", "x"]);
    assert_eq!(inverse_at(&t, 1), Some(1));
    t.md_session_end(doc);
}

#[test]
fn another_editor_on_different_lines_merges() {
    use crate::md::merge::{THEIRS, YOURS};
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n\ntwo\n\nthree\n");
    t.feed(b"after");
    let text = |t: &Terminal| String::from_utf8(t.linked_source(doc).unwrap().to_bytes()).unwrap();
    t.md_session_start(doc, at(0, 3));
    t.md_session_type(doc, "!");
    assert!(t.md_dirty(doc));
    // They change the last line and add one; our typing stays.
    assert!(t.md_reload(doc, b"one\n\ntwo\n\nTHREE\nfour\n"));
    assert_eq!(text(&t), "one!\n\ntwo\n\nTHREE\nfour\n");
    assert!(t.md_dirty(doc), "ours isn't in the file yet");
    assert!(!t.md_has_conflict(doc));
    assert_eq!(t.md_session_cursor(doc), Some(at(0, 4)), "the cursor didn't move");
    assert_eq!(inverse_at(&t, 1), Some(4));
    t.md_session_type(doc, "?");
    assert_eq!(text(&t), "one!?\n\ntwo\n\nTHREE\nfour\n");
    // They add above the cursor: it stays with its line.
    let cursor_line = |t: &Terminal| t.md_session_cursor(doc).unwrap().line;
    assert!(t.md_reload(doc, b"top\n\none\n\ntwo\n\nTHREE\nfour\n"));
    assert_eq!(text(&t), "top\n\none!?\n\ntwo\n\nTHREE\nfour\n");
    assert_eq!(cursor_line(&t), 2);
    // The file is the same again if they write what we have.
    assert!(t.md_reload(doc, text(&t).as_bytes()));
    assert!(!t.md_dirty(doc));
    // Both change one line: markers, in the document only.
    t.md_session_type(doc, "X");
    assert!(t.md_reload(doc, b"top\n\nrewritten\n\ntwo\n\nTHREE\nfour\n"));
    assert!(t.md_has_conflict(doc));
    assert!(text(&t).contains(YOURS) && text(&t).contains(THEIRS));
    assert!(text(&t).contains("one!?X") && text(&t).contains("rewritten"));
    assert!(t.md_dirty(doc));
    // Sorting it out by hand clears it.
    t.md_session_end(doc);
    assert!(t.md_session_start(doc, at(0, 0)));
    let lines = t.linked_source(doc).unwrap().lines().to_vec();
    let first = lines.iter().position(|l| l == YOURS).unwrap();
    let last = lines.iter().position(|l| l == THEIRS).unwrap();
    t.md_edit(doc, at(first, 0)..at(last, THEIRS.len()), "settled").unwrap();
    assert!(!t.md_has_conflict(doc));
    t.md_session_end(doc);
}

#[test]
fn a_clean_document_follows_the_file_by_its_changed_lines() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after");
    let bytes = |t: &Terminal| t.linked_source(doc).unwrap().to_bytes();
    for file in
        ["one\n\ntwo", "one\r\n\r\ntwo\r\n", "\n", "", "new", "new\nmore\n", "more\n", "a\nmore\n", "a\nb\nmore\nc\n"]
    {
        assert!(t.md_reload(doc, file.as_bytes()), "{file:?}");
        assert_eq!(String::from_utf8(bytes(&t)).unwrap(), file);
        assert!(!t.md_dirty(doc), "{file:?}");
        assert!(!t.md_reload(doc, file.as_bytes()), "{file:?} again");
        // As loading that text whole shows it.
        let mut fresh = term(40, 12);
        link_bytes(&mut fresh, file.as_bytes());
        fresh.feed(b"after");
        assert_eq!(shown_text(&t), shown_text(&fresh), "{file:?}");
    }
    // Saving: what we wrote coming back changes nothing.
    t.md_session_start(doc, at(0, 0));
    t.md_session_type(doc, "z");
    t.md_mark_saved(doc);
    let written = bytes(&t);
    assert!(!t.md_reload(doc, &written));
    t.md_session_end(doc);
}

#[test]
fn undoing_a_merge_leaves_the_typing_and_stays_dirty() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after");
    let text = |t: &Terminal| String::from_utf8(t.linked_source(doc).unwrap().to_bytes()).unwrap();
    t.md_session_start(doc, at(0, 3));
    t.md_session_type(doc, "!");
    assert!(t.md_reload(doc, b"one\n\nTWO\n"));
    assert_eq!(text(&t), "one!\n\nTWO\n");
    t.md_session_undo(doc, false);
    assert_eq!(text(&t), "one!\n\ntwo\n", "the merge goes first");
    t.md_session_undo(doc, false);
    assert_eq!(text(&t), "one\n\ntwo\n");
    assert!(t.md_dirty(doc), "the file says TWO");
    t.md_session_undo(doc, true);
    t.md_session_undo(doc, true);
    assert_eq!(text(&t), "one!\n\nTWO\n");
    t.md_session_end(doc);
}

#[test]
fn two_documents_of_one_file_take_each_others_writes() {
    let mut t = term(40, 20);
    let a = link(&mut t, "one\n\ntwo\n");
    t.feed(b"between\r\n");
    let b = link(&mut t, "one\n\ntwo\n");
    t.feed(b"after");
    let text = |t: &Terminal, d| String::from_utf8(t.linked_source(d).unwrap().to_bytes()).unwrap();
    t.md_session_start(a, at(0, 3));
    t.md_session_type(a, "A");
    t.md_mark_saved(a);
    t.md_session_end(a);
    // B has its own change when A's write arrives.
    t.md_edit(b, at(2, 3)..at(2, 3), "B").unwrap();
    let written = text(&t, a);
    assert!(!t.md_reload(a, written.as_bytes()), "its own write");
    assert!(t.md_reload(b, written.as_bytes()));
    assert_eq!(text(&t, b), "oneA\n\ntwoB\n");
    // B writes; A, clean, follows.
    t.md_mark_saved(b);
    assert!(t.md_reload(a, text(&t, b).as_bytes()));
    assert_eq!(text(&t, a), "oneA\n\ntwoB\n");
    assert!(!t.md_dirty(a) && !t.md_dirty(b));
}

#[test]
fn markers_the_file_already_has_are_no_conflict() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "<<<<<<< yours\nx\n>>>>>>> file\n");
    t.feed(b"after");
    t.md_edit(doc, at(1, 1)..at(1, 1), "y").unwrap();
    assert!(!t.md_has_conflict(doc));
}

#[test]
fn a_file_that_stops_being_text_keeps_the_typing() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n");
    t.feed(b"after");
    t.md_edit(doc, at(0, 3)..at(0, 3), "!").unwrap();
    assert!(!t.md_reload(doc, b"\xff\xfe"));
    assert!(t.md_has_conflict(doc));
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"one!\n");
}

#[test]
fn a_changed_document_hands_over_what_it_needs_to_merge() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "one\n");
    t.feed(b"after");
    t.md_edit(doc, at(0, 3)..at(0, 3), "!").unwrap();
    t.md_collect_dirty();
    let gone = t.take_discarded();
    assert_eq!(gone.len(), 1);
    assert_eq!(
        (gone[0].doc, gone[0].base.to_bytes(), gone[0].ours.to_bytes()),
        (doc, b"one\n".to_vec(), b"one!\n".to_vec())
    );
    t.md_collect_dirty();
    assert!(t.take_discarded().is_empty(), "once");
}

#[test]
fn a_table_too_wide_for_the_window_wraps_inside_its_columns() {
    let mut t = term(30, 20);
    t.set_markdown_styling(&[("md-table".into(), vec!["overflow=wrap".into()])]);
    let doc = link(
        &mut t,
        "| k | meaning |\n|---|---|\n| a | one two three four five six seven eight nine |\n| b | short |\n",
    );
    t.feed(b"after");
    let text = shown_text(&t);
    assert!(text.iter().position(|l| l == "after").unwrap() > 3, "more rows than table rows: {text:?}");
    // Every word is still there, in the second column of its row.
    let row = |y: usize| shown(&t, y).rich.as_ref().map(|r| r.fields.clone());
    let all = text.join(" ");
    for word in ["one", "five", "nine", "short"] {
        assert!(all.contains(word), "{word}: {text:?}");
    }
    assert_eq!(row(1).map(|f| f.len()), Some(1), "a field for each column");
    // With room again, each row is one line.
    t.resize(80, 20);
    assert_eq!(shown_text(&t).iter().position(|l| l == "after"), Some(3));
    t.resize(30, 20);
    assert!(shown_text(&t).iter().position(|l| l == "after").unwrap() > 3);
    // A cell's edit that makes the table fit, and one that makes it not.
    t.md_edit(doc, at(2, 6)..at(2, 50), "x").unwrap();
    assert_eq!(shown_text(&t).iter().position(|l| l == "after"), Some(3));
    t.md_edit(doc, at(3, 6)..at(3, 6), "a very long cell indeed which will not fit here");
    assert!(shown_text(&t).iter().position(|l| l == "after").unwrap() > 3);
}

#[test]
fn a_table_too_wide_for_the_window_keeps_a_line_a_row_and_follows_the_edit_cursor() {
    let mut t = term(30, 20);
    let src = "| k | meaning | more |\n|---|---|---|\n| a | one two three four five six seven eight nine | far |\n";
    let doc = link(&mut t, src);
    t.feed(b"after");
    assert_eq!(shown_text(&t).iter().position(|l| l == "after"), Some(2), "{:?}", shown_text(&t));
    let group = style_of(&t, 0).unwrap().group;
    assert_eq!(t.table_scroll(group), 0);
    // The edit cursor in the last column scrolls it into the window, and
    // back in the first, back to the left.
    let far = src.lines().nth(2).unwrap().find("far").unwrap();
    t.md_session_start(doc, crate::md::source::SourcePos { line: 2, byte: far });
    assert!(t.table_scroll(group) > 0, "scrolled to the last column");
    t.md_session_end(doc);
    t.md_session_start(doc, crate::md::source::SourcePos { line: 2, byte: 2 });
    assert_eq!(t.table_scroll(group), 0);
    t.md_session_end(doc);
    // Scrolling stays within the table.
    assert!(t.scroll_table(group, 50, 40));
    assert_eq!(t.table_scroll(group), 40);
    assert!(!t.scroll_table(group, 10, 40));
    assert!(t.scroll_table(group, -100, 40));
    assert_eq!(t.table_scroll(group), 0);
}

#[test]
fn a_table_arriving_row_by_row_wraps_its_earlier_rows_too() {
    let mut t = term(30, 20);
    t.set_markdown_styling(&[("md-table".into(), vec!["overflow=wrap".into()])]);
    t.feed(b"\x1b]7701;md\x1b\\| k | v |\n|---|---|\n| first row of it is long enough |  x |\n");
    let before = shown_text(&t);
    t.feed(b"| y | second row of it is even longer than the first |\n\x1b]7701;md-end\x1b\\after");
    let text = shown_text(&t);
    assert_ne!(&before[..2], &text[..2].to_vec()[..], "{text:?}");
    let all = text.join(" ");
    for word in ["first", "enough", "second", "longer"] {
        assert!(all.contains(word), "{word}: {text:?}");
    }
}

#[test]
fn a_row_short_of_cells_still_has_every_column() {
    let mut t = term(40, 10);
    link(&mut t, "| a | b | c |\n|---|---|---|\n| only |\n");
    t.feed(b"x");
    assert_eq!(shown(&t, 1).rich.as_ref().unwrap().field_count(), 3);
}

#[test]
fn the_cell_holding_the_edit_cursor_is_focused() {
    use crate::term::EditMove::*;
    let mut t = term(40, 12);
    let doc = link(&mut t, "| a | b |\n|---|---|\n| one | two |\n\nafter\n");
    t.feed(b"x");
    let focus = |t: &Terminal, y: usize| shown(t, y).rich.as_ref().and_then(|r| r.focus.as_ref()).and_then(|f| f.field);
    assert_eq!(focus(&t, 1), None);
    t.md_session_start(doc, at(2, 2));
    let rendered = (0..12).find(|&y| shown_text(&t)[y].starts_with("one")).unwrap();
    assert_eq!(focus(&t, rendered), Some(0), "over the first cell");
    t.md_session_move(doc, End);
    assert_eq!(focus(&t, rendered), Some(1), "past the closing pipe, still the last");
    for _ in 0..6 {
        t.md_session_move(doc, Left);
    }
    assert_eq!(focus(&t, rendered), Some(1), "just inside the second");
    t.md_session_move(doc, Left);
    t.md_session_move(doc, Left);
    assert_eq!(focus(&t, rendered), Some(0));
    t.md_session_end(doc);
    assert!((0..12).all(|y| focus(&t, y).is_none()), "gone with the session");
}

#[test]
fn the_focused_cell_is_marked_in_the_source_view() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "| a | b |\n|---|---|\n| one | two |\n");
    t.feed(b"x");
    // Nothing is marked unless asked for.
    t.md_session_start(doc, at(2, 9));
    let y = (0..12).find(|&y| shown_text(&t)[y] == "| one | two |").unwrap();
    let code = shown(&t, y).cells()[0].bg;
    assert!((0..13).all(|c| shown(&t, y).cells()[c].bg == code));
    t.md_session_end(doc);
    t.feed(b"\x1b]7701;def;md-edit-mark;bg=3\x1b\\");
    t.md_session_start(doc, at(2, 9));
    let row = shown(&t, y);
    let marked: Vec<bool> = row.cells()[..13].iter().map(|c| c.bg != crate::grid::Color::DEFAULT).collect();
    let code = row.cells()[0].bg;
    let hit = |c: usize| row.cells()[c].bg != code;
    assert!((8..12).all(hit), "the second cell, between its pipes: {marked:?}");
    assert!(!(0..7).any(hit) && !hit(12), "nothing else: {marked:?}");
    t.md_session_move(doc, crate::term::EditMove::Home);
    let row = shown(&t, y);
    assert_eq!(row.cells()[9].bg, code, "it moved");
    t.md_session_end(doc);
}

#[test]
fn a_rows_hover_range_leaves_out_its_source_view() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "| a |\n|---|\n| one |\n");
    t.feed(b"x");
    let first = t.grid().line_no(0);
    assert_eq!(t.md_table_cell(first + 1, 0).map(|c| c.end - c.start), Some(1));
    t.md_session_start(doc, at(2, 2));
    let c = t.md_table_cell(first + 1, 0).unwrap();
    assert_eq!((c.start, c.end), (first + 1, first + 2));
    assert_eq!(t.md_table_cell(first + 2, 0), None, "the box itself");
    t.md_session_end(doc);
}

#[test]
fn the_two_views_of_a_block_look_different() {
    let mut t = term(40, 12);
    let doc = link(&mut t, "some **bold** text\n\nafter\n");
    t.feed(b"x");
    let before = shown(&t, 0).rich.as_ref().map(|r| r.style.clone());
    t.md_session_start(doc, at(0, 0));
    let rendered = shown(&t, 0).rich.clone().unwrap();
    let focus = rendered.focus.clone().expect("the block as it reads is highlighted");
    assert_eq!(focus.field, None, "the whole row");
    assert_eq!(focus.style.band, crate::rich::builtin("md-edit").unwrap().band);
    let boxed = shown(&t, 1).style().cloned().unwrap();
    assert_eq!(boxed.rule, None, "the box isn't coloured unless asked");
    assert_ne!(boxed.band, focus.style.band);
    t.md_session_end(doc);
    assert_eq!(shown(&t, 0).rich.as_ref().map(|r| r.style.clone()), before);
    assert!(shown(&t, 0).rich.as_ref().is_none_or(|r| r.focus.is_none() && r.caret.is_none()));
}

#[test]
fn a_second_cursor_mirrors_the_first_in_the_text_as_it_reads() {
    use crate::term::EditMove::*;
    let mut t = term(40, 12);
    let doc = link(&mut t, "# Title\n\nsome **bold** [link](http://a.b) end\n\n| a | b |\n|---|---|\n| one |  |\n");
    t.feed(b"x");
    let caret = |t: &Terminal, y: usize| shown(t, y).rich.as_ref().and_then(|r| r.caret).map(|c| (c.col, c.after));
    // In the heading: the marker maps to the first letter.
    t.md_session_start(doc, at(0, 0));
    assert_eq!(caret(&t, 0), Some((0, false)));
    t.md_session_move(doc, End);
    assert_eq!(caret(&t, 0), Some((4, true)), "past the last letter");
    assert_eq!(caret(&t, 1), None, "not on the source too");
    // In a paragraph: markers and the address count for nothing.
    t.md_session_end(doc);
    let y = (0..12).find(|&y| shown_text(&t)[y].starts_with("some")).unwrap();
    t.md_session_start(doc, at(2, 7));
    assert_eq!(caret(&t, y), Some((5, false)), "inside ** it is the b");
    t.md_session_end(doc);
    t.md_session_start(doc, at(2, 8));
    assert_eq!(caret(&t, y), Some((6, false)), "the o of bold");
    t.md_session_end(doc);
    let line = t.linked_source(doc).unwrap().lines()[2].clone();
    t.md_session_start(doc, at(2, line.find("http").unwrap() + 2));
    assert_eq!(caret(&t, y), Some((14, false)), "in the address: what follows it");
    t.md_session_end(doc);
    // In a table cell, and an empty one.
    let row = (0..12).find(|&y| shown_text(&t)[y].starts_with("one")).unwrap();
    t.md_session_start(doc, at(6, 3));
    let c = shown(&t, row).rich.as_ref().and_then(|r| r.caret).unwrap();
    assert_eq!((c.col, c.field), (1, Some(0)));
    t.md_session_end(doc);
    t.md_session_start(doc, at(6, 8));
    let c = shown(&t, row).rich.as_ref().and_then(|r| r.caret).unwrap();
    assert_eq!(c.field, Some(1));
    t.md_session_end(doc);
}

/// A terminal with something 20 cells by 3 rows placed by `keys`.
fn flowing(cols: usize, rows: usize, keys: &str) -> Terminal {
    let mut t = term(cols, rows);
    t.set_cell_size(10, 20);
    let id = t.create_surface(200, 60);
    t.feed(format!("\x1b]7701;place;{id};min-text=10 {keys}\x1b\\").as_bytes());
    t
}

fn row_text(t: &Terminal, y: usize) -> String {
    t.grid().row(y).text().trim_end().to_owned()
}

#[test]
fn text_wraps_left_of_what_is_placed_on_the_right() {
    let mut t = flowing(40, 8, "place=right w=20 h=3 wrap=left");
    assert_eq!(pos(&t), (0, 0), "text goes on from its top");
    assert_eq!(t.text_cols(), 19, "with the gap");
    t.feed("x".repeat(19 * 3 + 5).as_bytes());
    for y in 0..3 {
        assert_eq!(row_text(&t, y), "x".repeat(19), "row {y}");
    }
    assert_eq!(row_text(&t, 3), "xxxxx", "the whole width below it");
    assert_eq!(t.text_cols(), 40);
}

#[test]
fn text_wraps_right_of_what_is_placed_on_the_left() {
    let mut t = flowing(40, 8, "place=left w=20 h=3 wrap=right");
    assert_eq!(pos(&t), (21, 0));
    t.feed(b"one\r\ntwo\n");
    assert_eq!(cell(&t, 21, 0).ch, 'o');
    assert_eq!(cell(&t, 21, 1).ch, 't');
    assert_eq!(pos(&t).0, 24, "a line feed alone stays in its column");
    t.feed(b"\r\nfour\r\nfive");
    assert_eq!(row_text(&t, 3), "four", "the left margin again, below it");
    assert_eq!(row_text(&t, 4), "five");
    assert_eq!(t.grid().row(3).free, None);
}

#[test]
fn programs_count_columns_in_what_is_free() {
    let mut t = flowing(40, 8, "place=left w=20 h=3 wrap=right");
    t.feed(b"abc\x1b[1Gz\x1b[6n");
    assert_eq!(row_text(&t, 0).trim_start(), "zbc");
    assert_eq!(cell(&t, 21, 0).ch, 'z');
    assert_eq!(replies(&mut t), "\x1b[1;2R");
    t.feed(b"\x1b[2;3Hq\x1b[18t");
    assert_eq!(cell(&t, 23, 1).ch, 'q');
    assert_eq!(replies(&mut t), "\x1b[8;8;19t");
    t.feed(b"\r\x08");
    assert_eq!(pos(&t), (21, 1), "backspace stops at its edge");
}

#[test]
fn both_sides_fill_in_turn_and_the_width_is_their_sum() {
    let mut t = flowing(60, 8, "place=center w=20 h=3 wrap=both");
    assert_eq!(t.text_cols(), 38);
    t.feed("a".repeat(19).as_bytes());
    t.feed("b".repeat(19).as_bytes());
    t.feed(b"c");
    assert_eq!(row_text(&t, 0), format!("{}{}{}", "a".repeat(19), " ".repeat(22), "b".repeat(19)));
    assert_eq!(row_text(&t, 1), "c");
}

#[test]
fn largest_takes_the_wider_side_and_is_the_default_for_at() {
    let t = flowing(60, 8, "at=5 w=20 h=3");
    assert_eq!(pos(&t), (26, 0));
    let t = flowing(60, 8, "at=35 w=20 h=3");
    assert_eq!((pos(&t), t.text_cols()), ((0, 0), 34));
}

#[test]
fn too_little_room_beside_it_falls_back_to_below() {
    let mut t = flowing(30, 8, "place=right w=20 h=3 wrap=left min-text=20");
    assert_eq!(pos(&t), (0, 3), "nine columns are less than min-text");
    t.feed(b"below");
    assert_eq!(row_text(&t, 3), "below");
    let t = flowing(30, 8, "place=right w=20 h=3 wrap=left min-text=5");
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn gap_is_left_between_text_and_what_is_placed() {
    let t = flowing(40, 8, "place=right w=20 h=3 wrap=left gap=4");
    assert_eq!(t.text_cols(), 16);
}

#[test]
fn text_scrolls_with_what_it_is_beside() {
    let mut t = flowing(40, 4, "place=right w=20 h=3 wrap=left");
    t.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");
    assert_eq!(t.text_cols(), 40, "it has scrolled away");
    t.feed("y".repeat(30).as_bytes());
    assert_eq!(row_text(&t, 3), "y".repeat(30));
}

#[test]
fn text_goes_around_a_fixed_placement_on_the_rows_it_covers() {
    let mut t = flowing(40, 8, "fixed=-1,2 w=20 h=3 wrap=left");
    assert_eq!(pos(&t), (0, 0));
    assert_eq!(t.text_cols(), 40, "above it, the whole width");
    t.feed("a".repeat(45).as_bytes());
    assert_eq!(row_text(&t, 0).len(), 40);
    t.feed(b"\r\n");
    t.feed("b".repeat(25).as_bytes());
    assert_eq!(row_text(&t, 2), "b".repeat(19), "wrapped short beside it");
    assert_eq!(row_text(&t, 3), "b".repeat(6));
    // Under it at the bottom of the screen, and the screen scrolling.
    t.feed(b"\x1b[8H\r\n\r\n\r\n");
    t.feed("c".repeat(30).as_bytes());
    assert_eq!(row_text(&t, 7), "c".repeat(30));
    // By default, text runs on under a fixed one.
    let mut t = flowing(40, 8, "fixed=0,0 w=20 h=3");
    t.feed("d".repeat(30).as_bytes());
    assert_eq!(row_text(&t, 0), "d".repeat(30));
}

#[test]
fn full_screen_programs_get_the_whole_width() {
    let mut t = flowing(40, 8, "place=right w=20 h=3 wrap=left");
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.text_cols(), 40);
    t.feed("z".repeat(30).as_bytes());
    assert_eq!(row_text(&t, 0), "z".repeat(30));
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.text_cols(), 19);
}

#[test]
fn text_beside_wraps_again_on_resize() {
    let mut t = flowing(40, 8, "place=right w=50% h=3 wrap=left");
    t.feed("x".repeat(19 * 3 + 5).as_bytes());
    t.feed(b"\r\nnext");
    t.resize(60, 8);
    let p = &t.graphics().placements()[0];
    assert_eq!((p.at.col, p.at.span.0), (30, 30), "a percentage of the new width");
    for y in 0..2 {
        assert_eq!(row_text(&t, y), "x".repeat(29), "row {y}");
    }
    assert_eq!(row_text(&t, 2), "x".repeat(4));
    assert_eq!(row_text(&t, 3), "next");
    assert_eq!(pos(&t), (4, 3));
    // Narrower: more rows beside it, the rest below them moving down.
    t.resize(50, 8);
    assert_eq!(row_text(&t, 0), "x".repeat(24));
    assert_eq!(row_text(&t, 2), "x".repeat(14));
    assert_eq!(row_text(&t, 3), "next");
    // Back: nothing lost.
    t.resize(40, 8);
    assert_eq!(row_text(&t, 3), "xxxxx");
    assert_eq!(row_text(&t, 4), "next");
}

#[test]
fn text_beside_on_the_right_wraps_again_on_resize_keeping_wide_characters() {
    let mut t = flowing(40, 8, "place=left w=20 h=3 wrap=right min-text=5");
    t.feed("日本語日本語日本語日本語".as_bytes());
    t.resize(30, 8);
    // Nine columns beside it: four wide characters to a row.
    assert_eq!(t.grid().row(0).free.map(|f| f.width()), Some(9));
    assert_eq!(t.grid().row(1).text().chars().filter(|c| *c != ' ').count(), 4);
    t.resize(44, 8);
    assert_eq!(t.grid().row(0).free.map(|f| f.start()), Some(21));
    assert_eq!(cell(&t, 21, 0).ch, '日');
    assert!(!cell(&t, 42, 0).flags.contains(Flags::WIDE) || cell(&t, 43, 0).flags.contains(Flags::WIDE_SPACER));
}

#[test]
fn markdown_lines_are_laid_out_beside_an_image() {
    use crate::graphics::Reader;
    use std::sync::Arc;
    let mut t = term(50, 12);
    t.set_cell_size(10, 20);
    let png = crate::image::png::encode(200, 40, &[128; 200 * 40 * 3]);
    t.set_graphics_reader(Some(Reader(Arc::new(move |_| Ok(png.clone())))));
    let md = "![A](/a.png){place=left w=20 h=2 wrap=right}\nnext to it, and long enough that it needs more than one line to fit\n\nbelow it\n";
    t.feed(format!("\x1b]7701;md;base=/\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    t.wait_images();
    let area =
        |t: &Terminal, y: usize| t.md_line_of(t.grid().row(y)).and_then(|l| l.rich.as_ref()).and_then(|r| r.area);
    assert_eq!(area(&t, 0), Some((21, 29)), "in the free columns");
    let fits = |y: usize| t.md_line_of(t.grid().row(y)).is_some_and(|l| l.text().trim_end().len() <= 29);
    assert!(fits(0) && fits(1));
    assert_eq!(area(&t, 4), None, "the whole width after it");
    // Wider: laid out again, beside it still.
    t.resize(70, 12);
    assert_eq!(area(&t, 0), Some((21, 49)));
}

fn block_at(t: &Terminal) -> (usize, (usize, usize)) {
    let p = t.graphics().placements().iter().find(|p| p.block.is_some()).unwrap();
    (p.at.col, p.at.span)
}

#[test]
fn a_markdown_region_can_be_a_block_with_text_beside_it() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"before\r\n");
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=left\x1b\\one\n\ntwo\n\nthree\n\x1b]7701;md-end\x1b\\");
    let (col, (w, rows)) = block_at(&t);
    assert_eq!((col, w), (40, 20));
    assert_eq!(rows, 5, "a row per line, blank ones too");
    assert_eq!(pos(&t), (0, 1), "text goes on from its top");
    assert_eq!(t.text_cols(), 39);
    assert_eq!(row_text(&t, 1), "", "its lines are not on the grid's rows");
    t.feed("x".repeat(45).as_bytes());
    assert_eq!(row_text(&t, 1), "x".repeat(39));
    assert_eq!(row_text(&t, 2), "x".repeat(6));
}

#[test]
fn a_block_grows_a_row_per_line_while_open_rewrapping_text_beside_it() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=left\x1b\\a\n\n");
    assert_eq!(block_at(&t).1.1, 2, "the line, and the blank one after it");
    t.feed(b"b\n\nc\n\n");
    assert_eq!(block_at(&t).1.1, 6);
    t.feed(b"\x1b]7701;md-end\x1b\\");
    // Text beside a block that opens again is not this one's: a new region.
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn text_written_while_a_block_is_open_wraps_as_it_grows() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=left\x1b\\a\n\n");
    // A raw line goes to the console, beside the block.
    t.feed(format!("{}\n", "\x1b[31mxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\x1b[m").as_bytes());
    let before = block_at(&t).1.1;
    t.feed(b"\nb\n\nc\n\nd\n\ne\n\n");
    assert!(block_at(&t).1.1 > before, "grew");
    t.feed(b"\x1b]7701;md-end\x1b\\");
    let wide = (0..12).map(|y| row_text(&t, y)).filter(|r| r.len() > 39).count();
    assert_eq!(wide, 0, "nothing beside it reaches under it: {:?}", row_text(&t, 0));
}

#[test]
fn a_block_scrolls_the_screen_to_show_itself() {
    let mut t = term(60, 6);
    t.set_cell_size(10, 20);
    t.feed(&b"line\r\n".repeat(5));
    assert_eq!(pos(&t).1, 5);
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=left\x1b\\");
    t.feed(&b"a\n\n".repeat(3));
    t.feed(b"\x1b]7701;md-end\x1b\\");
    assert_eq!(block_at(&t).1.1, 6);
    assert_eq!(pos(&t).1, 0, "the cursor went up with its row, to show them");
}

#[test]
fn a_block_that_skips_has_text_resume_below_it() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=20\x1b\\a\n\nb\n\x1b]7701;md-end\x1b\\");
    assert_eq!(pos(&t), (0, 3));
    t.feed(b"next");
    assert_eq!(row_text(&t, 3), "next");
}

#[test]
fn a_block_with_wrap_none_lets_text_run_on_at_full_width() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=none\x1b\\a\n\x1b]7701;md-end\x1b\\");
    assert_eq!(t.text_cols(), 60);
    t.feed("x".repeat(50).as_bytes());
    assert_eq!(row_text(&t, 0), "x".repeat(50));
}

#[test]
fn a_blocks_lines_are_laid_out_at_its_width() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    let long = "word ".repeat(12);
    t.feed(format!("\x1b]7701;md;place=left w=20 wrap=right\x1b\\{long}\n\x1b]7701;md-end\x1b\\").as_bytes());
    let doc = t.graphics().placements().iter().find_map(|p| p.block).unwrap();
    let lines = t.md_doc_lines(doc).unwrap();
    assert!(lines.len() > 1 && lines.iter().all(|l| l.text().trim_end().len() <= 20), "{}", lines.len());
    assert_eq!(block_at(&t).1.1, lines.len());
}

#[test]
fn a_block_with_auto_width_fits_its_lines_up_to_its_limit() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right max-w=30 wrap=left\x1b\\short\n\n");
    let (col, (w, _)) = block_at(&t);
    assert_eq!((col, w), (55, 5));
    t.feed(b"a longer line\n\n");
    let (col, (w, _)) = block_at(&t);
    assert_eq!((col, w), (47, 13), "wider, as its lines grow");
    t.feed(format!("{}\n\x1b]7701;md-end\x1b\\", "word ".repeat(20)).as_bytes());
    assert_eq!(block_at(&t).1.0, 30, "its limit");
}

#[test]
fn block_keys_left_out_come_from_the_markdown_styling() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.set_markdown_styling(&[(BLOCK_CLASS.into(), vec!["place=right".into(), "w=30".into(), "wrap=left".into()])]);
    t.feed(b"\x1b]7701;md\x1b\\a\n\x1b]7701;md-end\x1b\\");
    assert_eq!(block_at(&t), (30, (30, 1)));
    t.feed(b"\x1b]7701;md;w=10\x1b\\a\n\x1b]7701;md-end\x1b\\");
    let p = t.graphics().placements().iter().rfind(|p| p.block.is_some()).unwrap();
    assert_eq!(p.at.span.0, 10);
    t.feed(b"\x1b]7701;md;place=full\x1b\\plain\n\x1b]7701;md-end\x1b\\");
    assert_eq!(t.graphics().placements().iter().filter(|p| p.block.is_some()).count(), 2);
}

#[test]
fn a_bad_key_leaves_the_region_on_rows_of_the_text() {
    let mut t = term(60, 12);
    t.feed(b"\x1b]7701;md;place=sideways\x1b\\plain\n\x1b]7701;md-end\x1b\\");
    assert!(t.graphics().placements().is_empty());
    assert!(t.md_line_of(t.grid().row(0)).is_some());
}

#[test]
fn a_base_after_keys_takes_the_rest() {
    let (keys, base, _) = super::rich::md_args_for_test("md;place=right w=40%;base=/a;b");
    assert_eq!(keys, ["place=right", "w=40%"]);
    assert_eq!(base.as_deref(), Some("/a;b"));
    assert_eq!(super::rich::md_args_for_test("md;base=/x").1.as_deref(), Some("/x"));
    assert!(super::rich::md_args_for_test("md").0.is_empty());
}

#[test]
fn a_block_is_laid_out_again_on_resize() {
    let mut t = term(100, 12);
    t.set_cell_size(10, 20);
    let long = "word ".repeat(16);
    t.feed(format!("\x1b]7701;md;place=right w=40% wrap=left\x1b\\{long}\n\x1b]7701;md-end\x1b\\").as_bytes());
    assert_eq!(block_at(&t), (60, (40, 2)));
    t.feed("x".repeat(70).as_bytes());
    t.resize(50, 12);
    let (col, (w, rows)) = block_at(&t);
    assert_eq!((col, w), (30, 20));
    assert_eq!(rows, 4, "narrower, so longer");
    // Twenty-nine beside it, over its rows.
    assert_eq!(row_text(&t, 0), "x".repeat(29));
    assert_eq!(row_text(&t, 1), "x".repeat(29));
    assert_eq!(row_text(&t, 2), "x".repeat(12));
}

#[test]
fn a_fixed_block_stays_on_its_cell_with_a_height() {
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;fixed=-1,0 w=20 h=3 wrap=left\x1b\\one\n\ntwo\n\nthree\n\nfour\n\x1b]7701;md-end\x1b\\");
    let p = t.graphics().placements().iter().find(|p| p.block.is_some()).unwrap().clone();
    assert_eq!(p.at.span, (20, 3), "its height, however many lines");
    assert_eq!(p.fixed_cell((60, 12)), Some((40, 0)));
    assert_eq!(pos(&t), (0, 0));
    assert_eq!(t.text_cols(), 39, "text flows on the rows it covers");
    t.feed(&b"line\r\n".repeat(30));
    assert_eq!(t.graphics().placements().iter().filter(|p| p.block.is_some()).count(), 1, "scrolling keeps it");
    assert_eq!(t.text_cols(), 60, "below it, the whole width");
    // Full-screen programs: there, with the width theirs.
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.graphics().placements().iter().filter(|p| p.block.is_some()).count(), 1);
    assert_eq!(t.text_cols(), 60);
    t.feed(b"\x1b[?1049l");
}

#[test]
fn a_fixed_block_closes_by_its_program_the_user_or_a_reset() {
    let open = b"\x1b]7701;md;fixed=0,0 w=20 h=3 wrap=right\x1b\\a\n\x1b]7701;md-end\x1b\\";
    let count = |t: &Terminal| t.graphics().placements().iter().filter(|p| p.block.is_some()).count();
    let mut t = term(60, 12);
    t.set_cell_size(10, 20);
    t.feed(open);
    assert_eq!(count(&t), 1);
    t.feed(b"\x1b]7701;close-block\x1b\\");
    assert_eq!((count(&t), t.text_cols()), (0, 60));
    t.feed(open);
    assert!(t.close_block(), "the user dismissing it");
    assert!(!t.close_block());
    t.feed(open);
    t.feed(b"\x1bc");
    assert_eq!(count(&t), 0);
    // Tied to its line: it isn't closed, it scrolls away.
    t.feed(b"\x1b]7701;md;place=right w=20 wrap=left\x1b\\a\n\x1b]7701;md-end\x1b\\");
    assert!(!t.close_block());
}

#[test]
fn a_block_shows_the_end_of_what_outgrows_its_height() {
    use crate::render::{FrameParams, build_frame};
    let mut t = term(60, 12);
    t.set_cell_size(8, 16);
    let m = crate::font::CellMetrics { width: 8, height: 16, baseline: 12, ..Default::default() };
    t.feed(b"\x1b]7701;md;fixed=-1,0 w=20 h=3 wrap=none\x1b\\");
    for i in 0..9 {
        t.feed(format!("line{i}\n\n").as_bytes());
    }
    t.feed(b"\x1b]7701;md-end\x1b\\");
    let p = FrameParams {
        width: 480,
        height: 192,
        origin: (0, 0),
        scroll_offset: 0,
        focused: true,
        blink_on: true,
        selection: None,
        link: None,
        table_cell: None,
    };
    let f = build_frame(&t, m, &p);
    let o = &f.overlays[0];
    assert_eq!(o.clip, (320, 0, 480, 48));
    let last: String = o.rows.iter().rev().flat_map(|r| r.glyphs.iter().map(|g| g.ch)).take(5).collect();
    assert!(last.contains('8'), "the end shows: {last}");
    assert!(o.fill.is_some(), "over the text");
}

#[test]
fn a_block_opening_beside_another_takes_the_space_left_or_goes_below() {
    let mut t = term(100, 20);
    t.set_cell_size(10, 20);
    let two = |t: &Terminal| -> Vec<(usize, usize, u64)> {
        let blocks = t.graphics().placements().iter().filter(|p| p.block.is_some());
        blocks.map(|p| (p.at.col, p.at.span.0, p.at.line)).collect()
    };
    t.feed(b"\x1b]7701;md;place=right w=40 wrap=left\x1b\\a\n\nb\n\nc\n\x1b]7701;md-end\x1b\\");
    t.feed(b"\x1b]7701;md;place=right w=30 wrap=left\x1b\\z\n\x1b]7701;md-end\x1b\\");
    assert_eq!(two(&t)[1], (29, 30, 0), "beside the first, its gap between them");
    assert_eq!(t.text_cols(), 28);
    // No room: below it. (Fifty asked for, twenty-eight left, more than
    // min-text: it takes what's there.)
    t.feed(b"\x1b]7701;md;place=left w=50 wrap=right\x1b\\z\n\x1b]7701;md-end\x1b\\");
    assert_eq!(two(&t)[2], (0, 28, 0));
    let mut t = term(60, 20);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=50 wrap=none\x1b\\a\n\nb\n\nc\n\x1b]7701;md-end\x1b\\");
    t.feed(b"\x1b]7701;md;place=left w=30 wrap=none\x1b\\z\n\x1b]7701;md-end\x1b\\");
    assert_eq!(two(&t)[1].2, 5, "too little beside it");
    // Sizes follow the window.
    let mut t = term(100, 20);
    t.set_cell_size(10, 20);
    t.feed(b"\x1b]7701;md;place=right w=40% wrap=left\x1b\\a\n\x1b]7701;md-end\x1b\\");
    t.feed(b"\x1b]7701;md;place=left w=20% wrap=right\x1b\\z\n\x1b]7701;md-end\x1b\\");
    assert_eq!(two(&t), [(60, 40, 0), (0, 20, 0)]);
    t.resize(200, 20);
    assert_eq!(two(&t), [(120, 80, 0), (0, 40, 0)]);
    assert_eq!(t.text_cols(), 200 - 80 - 40 - 2);
}

fn md_section(t: &mut Terminal, keys: &str, text: &str) {
    t.feed(format!("\x1b]7701;md{keys}\x1b\\{text}\x1b]7701;md-end\x1b\\").as_bytes());
}

/// The document lines the screen shows, and its console rows, as text.
fn doc_text(t: &Terminal) -> Vec<String> {
    (0..t.rows())
        .map(|y| {
            let row = t.grid().row(y);
            t.md_line_of(row).unwrap_or(row).text().trim_end().to_owned()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn anchors(t: &Terminal) -> Vec<(u32, u32)> {
    (0..t.rows()).filter_map(|y| t.grid().row(y).rich.as_ref()?.anchor).collect()
}

#[test]
fn regions_add_sections_to_a_document_with_console_rows_between() {
    let mut t = term(40, 12);
    md_section(&mut t, "", "# One\n");
    t.feed(b"console\r\n");
    md_section(&mut t, "", "# Two\n");
    assert_eq!(doc_text(&t), ["One", "console", "Two"]);
    let a = anchors(&t);
    assert_eq!(a[0].0, a[1].0, "the same document");
    assert_eq!((a[0].1, a[1].1), (0, 1), "its lines in order");

    md_section(&mut t, ";doc=report", "# Three\n");
    md_section(&mut t, "", "# Four\n");
    let a = anchors(&t);
    assert_ne!(a[2].0, a[0].0, "named");
    assert_eq!(a[3].0, a[2].0, "without doc=, the last one used");
    assert_eq!(a[3].1, 1);
    md_section(&mut t, ";doc=", "# Five\n");
    assert_eq!(anchors(&t)[4].0, a[0].0, "the default one by its empty name");
}

#[test]
fn sections_are_laid_out_again_in_their_places() {
    let mut t = term(40, 12);
    t.set_cell_size(10, 20);
    let long = "word ".repeat(15);
    md_section(&mut t, "", &format!("{long}\n"));
    t.feed(b"console\r\n");
    md_section(&mut t, "", &format!("{long}\n"));
    let before = doc_text(&t).len();
    t.resize(20, 12);
    let after = doc_text(&t);
    assert!(after.len() > before);
    let at = after.iter().position(|l| l == "console").unwrap();
    assert!(at > 1 && at + 2 < after.len(), "between them: {after:?}");
}

#[test]
fn a_table_widening_in_a_later_section_keeps_console_rows_in_place() {
    let mut t = term(60, 14);
    t.set_cell_size(10, 20);
    md_section(&mut t, "", "| a | b |\n|:--|:--|\n| 1 | 2 |\n");
    t.feed(b"console\r\n");
    let long = "word ".repeat(30);
    md_section(&mut t, "", &format!("| {long} | {long} |\n"));
    let text = doc_text(&t);
    let at = text.iter().position(|l| l == "console").unwrap();
    assert!(text[at + 1..].iter().any(|l| l.contains("word")), "{text:?}");
    assert!(text[..at].iter().any(|l| l.contains('1')), "{text:?}");
}

#[test]
fn table_rows_starting_a_section_continue_the_table_with_as_many_columns() {
    let mut t = term(60, 14);
    t.set_cell_size(10, 20);
    md_section(&mut t, "", "| a | b |\n|:--|:--|\n| 1 | 2 |\n");
    t.feed(b"console\r\n");
    md_section(&mut t, "", "| 3 | 4 |\n");
    let groups: Vec<u32> = (0..14)
        .filter_map(|y| {
            let row = t.grid().row(y);
            let line = t.md_line_of(row)?;
            line.text().contains(['1', '3']).then(|| line.style().map_or(0, |s| s.group))
        })
        .collect();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0], groups[1], "shared columns");
    assert_eq!(doc_text(&t).iter().filter(|l| l.contains('a')).count(), 1, "no second header");
    // A new header is a new table; other columns too.
    md_section(&mut t, "", "| x | y |\n|:--|:--|\n| 5 | 6 |\n");
    md_section(&mut t, "", "| only |\n");
    let text = doc_text(&t);
    assert!(text.iter().any(|l| l.contains('x')) && text.iter().any(|l| l == "| only |"), "{text:?}");
}

#[test]
fn heading_numbers_run_through_a_document_when_styled() {
    let mut t = term(60, 14);
    let number = |s: &str| (s.to_owned(), vec!["number=on".to_owned()]);
    t.set_markdown_styling(&[number("md-h1"), number("md-h2")]);
    md_section(&mut t, "", "# First\n## Part\n## Part\n");
    t.feed(b"console\r\n");
    md_section(&mut t, "", "## Part\n# Second\n## Part\n");
    assert_eq!(doc_text(&t), ["1 First", "1.1 Part", "1.2 Part", "console", "1.3 Part", "2 Second", "2.1 Part"]);
    // Starting again: counters back to zero, what's shown stays.
    t.feed(b"\x1b]7701;md-reset\x1b\\");
    md_section(&mut t, "", "# Again\n");
    assert_eq!(doc_text(&t).last().unwrap(), "1 Again");
    assert_eq!(doc_text(&t)[0], "1 First");
    // Off by default, and by name.
    let mut t = term(60, 14);
    md_section(&mut t, "", "# Plain\n");
    assert_eq!(doc_text(&t), ["Plain"]);
}

#[test]
fn md_reset_starts_the_named_document_again() {
    let mut t = term(60, 14);
    t.set_markdown_styling(&[("md-h1".to_owned(), vec!["number=on".to_owned()])]);
    md_section(&mut t, ";doc=a", "# A\n");
    md_section(&mut t, ";doc=b", "# B\n");
    t.feed(b"\x1b]7701;md-reset;doc=a\x1b\\");
    md_section(&mut t, ";doc=b", "# B\n");
    md_section(&mut t, ";doc=a", "# A\n");
    assert_eq!(doc_text(&t), ["1 A", "1 B", "2 B", "1 A"]);
}

#[test]
fn an_ordered_list_goes_on_in_the_next_section() {
    let mut t = term(60, 14);
    md_section(&mut t, "", "1. one\n2. two\n");
    t.feed(b"console\r\n");
    md_section(&mut t, "", "1. three\n1. four\n");
    md_section(&mut t, "", "Text.\n\n1. fresh\n");
    assert_eq!(doc_text(&t)[..6], ["1. one", "2. two", "console", "3. three", "4. four", "Text."]);
    assert_eq!(doc_text(&t).last().unwrap(), "1. fresh");
}

#[test]
fn footnotes_number_on_through_a_document() {
    let mut t = term(60, 14);
    md_section(&mut t, "", "First[^1]\n\n[^1]: Note one.\n");
    md_section(&mut t, "", "Second[^1]\n\n[^1]: Note two.\n");
    let text = doc_text(&t);
    assert!(text.contains(&"First¹".to_owned()) && text.contains(&"Second²".to_owned()), "{text:?}");
    assert!(text.contains(&"¹ Note one.".to_owned()) && text.contains(&"² Note two.".to_owned()), "{text:?}");
}

#[test]
fn link_references_are_known_to_later_sections_and_earlier_lines() {
    let mut t = term(60, 14);
    md_section(&mut t, "", "See [the site][home] and [home].\n\n[home]: https://example.org/\n");
    let row = (0..14).find_map(|y| t.md_line_of(t.grid().row(y)).filter(|l| l.text().contains("site"))).unwrap();
    assert!(row.text().starts_with("See the site and home."), "{}", row.text());
    assert!(row.extra(4).is_some_and(|e| e.hyperlink != 0), "a line laid out before the definition");
    md_section(&mut t, "", "Again [home].\n");
    let row = (0..14).find_map(|y| t.md_line_of(t.grid().row(y)).filter(|l| l.text().contains("Again"))).unwrap();
    assert!(row.extra(6).is_some_and(|e| e.hyperlink != 0), "a later section");
    assert_eq!(doc_text(&t).iter().filter(|l| l.contains("example.org")).count(), 0, "the definition doesn't show");
}

#[test]
fn a_report_built_in_sections_reads_as_one_region_does() {
    let rows = |t: &Terminal| -> Vec<String> {
        (0..t.rows())
            .map(|y| {
                let row = t.grid().row(y);
                t.md_line_of(row).unwrap_or(row).text().trim_end().to_owned()
            })
            .collect()
    };
    let parts = [
        "# Report\n\nSee [site] first.\n\n1. one\n2. two\n",
        "1. three\n\n[site]: https://example.org/\n",
        "## Table\n\n| a | b |\n|---|---|\n| c | d |\n",
        "| e | f |\n",
    ];
    let mut whole = term(60, 20);
    md_section(
        &mut whole,
        ";doc=report",
        "# Report\n\nSee [site] first.\n\n1. one\n2. two\n3. three\n\n[site]: https://example.org/\n\n## Table\n\n| a | b |\n|---|---|\n| c | d |\n| e | f |\n",
    );
    let mut built = term(60, 20);
    md_section(&mut built, ";doc=report", parts[0]);
    for part in &parts[1..] {
        md_section(&mut built, "", part);
    }
    assert_eq!(rows(&built), rows(&whole));

    let mut t = term(60, 6);
    md_section(&mut t, "", "[a]: https://a.example/\n\n[b]: https://b.example/\n\n# Title\n");
    assert_eq!(rows(&t)[0], "Title", "definitions and the blank lines after them show nothing");
}

#[test]
fn a_reset_is_a_clean_restart_for_documents() {
    let mut t = term(60, 14);
    t.set_markdown_styling(&[("md-h1".to_owned(), vec!["number=on".to_owned()])]);
    md_section(&mut t, ";doc=a", "# A\n");
    t.feed(b"\x1bc");
    assert!(doc_text(&t).is_empty());
    md_section(&mut t, "", "# Fresh\n");
    assert_eq!(doc_text(&t), ["1 Fresh"], "a default document, its counters at zero");
}

#[test]
fn lines_leaving_the_scrollback_leave_the_document() {
    let mut t = Terminal::new(40, 5, 10, Palette::default());
    for i in 0..600 {
        md_section(&mut t, "", &format!("line {i}\n"));
    }
    let doc = anchors(&t)[0].0;
    let n = t.md_doc_lines(doc).unwrap().len();
    assert!(n < 400, "{n} lines kept");
    assert_eq!(doc_text(&t).last().unwrap(), "line 599", "what's there still shows");
    t.resize(30, 5);
    assert_eq!(doc_text(&t).last().unwrap(), "line 599");
}

#[test]
fn an_image_in_a_markdown_block_takes_lines_of_its_own() {
    use crate::graphics::Reader;
    use std::sync::Arc;
    let mut t = term(40, 20);
    t.set_cell_size(10, 20);
    // 100×40: 10 cells by 2 rows at its own size.
    let png = crate::image::png::encode(100, 40, &[128; 100 * 40 * 3]);
    t.set_graphics_reader(Some(Reader(Arc::new(move |r| match r.path.as_str() {
        "/d/a.png" => Ok(png.clone()),
        _ => Err("ENOENT".into()),
    }))));
    t.feed(b"\x1b]7701;md;place=right w=50% wrap=left;base=/d/\x1b\\one\n\n![a](a.png)\n\ntwo\n\x1b]7701;md-end\x1b\\");
    t.wait_images();
    let block = |t: &Terminal| t.graphics().placements().iter().find(|p| p.block.is_some()).unwrap().clone();
    let doc = block(&t).block.unwrap();
    let images = |t: &Terminal| -> Vec<(usize, usize, (usize, usize))> {
        t.md_block_images(doc).iter().map(|(line, p)| (*line, p.at.col, p.at.span)).collect()
    };
    let lines = |t: &Terminal| -> Vec<String> {
        t.md_doc_lines(doc).unwrap().iter().map(|l| l.text().trim_end().to_owned()).collect()
    };
    let at = images(&t)[0].0;
    assert_eq!(images(&t), [(at, 0, (10, 2))]);
    assert_eq!(lines(&t)[at..at + 3], ["", "", ""], "two for it, then the blank line after it");
    assert_eq!(lines(&t).last().unwrap(), "two");
    assert_eq!(block(&t).at.span.1, lines(&t).len(), "the block is as tall as its lines");
    assert!(t.graphics().image(t.md_block_images(doc)[0].1.key).is_some_and(|i| !i.rgba.is_empty()));

    // A narrower window: too wide for the block now, so smaller and fewer rows.
    t.resize(12, 20);
    let width = block(&t).at.cols as usize;
    assert!(width < 10, "the test's premise: the block is narrower than the image");
    let (_, _, (cols, rows)) = images(&t)[0];
    assert!(cols == width && rows == 1, "{cols}×{rows} in {width}");
    // And back: from the pixels it kept.
    t.resize(40, 20);
    assert_eq!(images(&t)[0].2, (10, 2));

    // Closing a fixed block takes its images along.
    let placed = t.graphics().placements().len();
    t.feed(b"\x1b]7701;md;fixed=0,0 w=20 h=6;base=/d/\x1b\\![a](a.png)\n\x1b]7701;md-end\x1b\\");
    t.wait_images();
    assert_eq!(t.graphics().placements().len(), placed + 2);
    assert!(t.close_block());
    assert_eq!(t.graphics().placements().len(), placed);
}

#[test]
fn an_image_in_a_markdown_block_that_fails_shows_as_text() {
    use crate::graphics::Reader;
    use std::sync::Arc;
    let mut t = term(40, 20);
    t.set_cell_size(10, 20);
    // The header reads, and so the rows are held; the file doesn't decode.
    let mut png = crate::image::png::encode(100, 40, &[128; 100 * 40 * 3]);
    png.truncate(crate::image::png::HEADER);
    t.set_graphics_reader(Some(Reader(Arc::new(move |r| match r.path.as_str() {
        "/d/bad.png" => Ok(png.clone()),
        _ => Err("ENOENT".into()),
    }))));
    t.feed(b"\x1b]7701;md;place=right w=20;base=/d/\x1b\\![bad](bad.png)\n\n![gone](gone.png)\n\x1b]7701;md-end\x1b\\");
    t.wait_images();
    let doc = t.graphics().placements().iter().find_map(|p| p.block).unwrap();
    let text: String = t.md_doc_lines(doc).unwrap().iter().map(|l| l.text()).collect();
    assert!(text.contains("bad") && text.contains("gone"), "{text:?}");
    assert!(t.md_block_images(doc).is_empty());
}

#[test]
fn images_with_text_beside_them_go_beside_each_other() {
    use crate::graphics::Reader;
    use std::sync::Arc;
    let mut t = term(40, 20);
    t.set_cell_size(10, 20);
    // 80×40: 8 cells by 2 rows.
    let png = crate::image::png::encode(80, 40, &[128; 80 * 40 * 3]);
    t.set_graphics_reader(Some(Reader(Arc::new(move |_| Ok(png.clone())))));
    let md = "![a](a.png){place=right wrap=left gap=0}\n![b](a.png){place=right wrap=left gap=0}\n";
    t.feed(format!("\x1b]7701;md;base=/d/\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    t.wait_images();
    let placed = |t: &Terminal| -> Vec<(u64, usize, usize)> {
        let top = t.graphics().placements().first().map_or(0, |p| p.at.line);
        t.graphics().placements().iter().map(|p| (p.at.line - top, p.at.col, p.at.span.0)).collect()
    };
    assert_eq!(placed(&t), [(0, 32, 8), (0, 24, 8)], "the second beside the first");

    // Narrower: both stay clear of each other.
    t.resize(20, 20);
    assert_eq!(placed(&t), [(0, 12, 8), (0, 4, 8)]);
    t.resize(40, 20);

    // No room left beside them: below.
    let md = "![c](a.png){place=right wrap=left gap=0 w=30}\n";
    t.feed(format!("\x1b]7701;md;base=/d/\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
    t.wait_images();
    let all = placed(&t);
    assert!(all[2].0 >= 2 && all[2].1 == 10, "{all:?}");
}

/// Keys for the editor as vim writes them: `Esc`, `CR`, `C-r`, otherwise each
/// character, the way a keyboard reports it.
fn vi_keys(
    t: &mut Terminal,
    vi: &mut crate::vi::Vi,
    doc: u32,
    bindings: &crate::keybind::Bindings,
    keys: &str,
) -> Vec<crate::vi::Outcome> {
    use crate::input::{Key, Mods};
    use crate::keybind::Trigger;
    let ctrl = Mods { ctrl: true, ..Mods::NONE };
    let mut out = Vec::new();
    let mut rest = keys;
    while let Some(c) = rest.chars().next() {
        let (ways, text): (Vec<Trigger>, Option<String>) = if let Some(r) = rest.strip_prefix("<Esc>") {
            rest = r;
            (vec![Trigger { key: Key::Escape, mods: Mods::NONE }], None)
        } else if let Some(r) = rest.strip_prefix("<CR>") {
            rest = r;
            (vec![Trigger { key: Key::Enter, mods: Mods::NONE }], None)
        } else if let Some(r) = rest.strip_prefix("<BS>") {
            rest = r;
            (vec![Trigger { key: Key::Backspace, mods: Mods::NONE }], None)
        } else if let Some(r) = rest.strip_prefix("<Down>") {
            rest = r;
            (vec![Trigger { key: Key::Down, mods: Mods::NONE }], None)
        } else if let Some(r) = rest.strip_prefix("<C-r>") {
            rest = r;
            (vec![Trigger { key: Key::Char('r'), mods: ctrl }], None)
        } else {
            rest = &rest[c.len_utf8()..];
            let ways = if c.is_uppercase() {
                vec![Trigger { key: Key::Char(c.to_ascii_lowercase()), mods: Mods::SHIFT }]
            } else if c.is_alphanumeric() || c == ' ' {
                vec![Trigger { key: Key::Char(c), mods: Mods::NONE }]
            } else {
                // As typed on another key with Shift, then as produced.
                vec![
                    Trigger { key: Key::Char('\u{1}'), mods: Mods::SHIFT },
                    Trigger { key: Key::Char(c), mods: Mods::NONE },
                ]
            };
            (ways, Some(c.to_string()))
        };
        out.push(vi.key(t, doc, bindings, &ways, text.as_deref()));
    }
    out
}

/// `text` with `keys` pressed from Normal mode at its start: what it becomes,
/// and where the cursor ends.
fn vi_run(text: &str, keys: &str) -> (String, (usize, usize)) {
    let mut t = term(60, 30);
    let doc = link(&mut t, text);
    assert!(t.md_session_start(doc, at(0, 0)));
    let mut vi = crate::vi::Vi::default();
    vi.start(&mut t, doc);
    let bindings = crate::keybind::Bindings::new(&[]);
    vi_keys(&mut t, &mut vi, doc, &bindings, keys);
    let bytes = t.linked_source(doc).unwrap().to_bytes();
    let cursor = t.md_session_cursor(doc).unwrap();
    (String::from_utf8(bytes).unwrap(), (cursor.line, cursor.byte))
}

#[test]
fn vi_moves() {
    let text = "one two.three\n\nfour  five\nsix\n";
    let cursor = |keys: &str| vi_run(text, keys).1;
    assert_eq!(cursor("l"), (0, 1));
    assert_eq!(cursor("3l"), (0, 3));
    assert_eq!(cursor("99l"), (0, 12), "on the last character, not past it");
    assert_eq!(cursor("$"), (0, 12));
    assert_eq!(cursor("$h"), (0, 11));
    assert_eq!(cursor("$0"), (0, 0));
    assert_eq!(cursor("w"), (0, 4));
    assert_eq!(cursor("ww"), (0, 7), "punctuation is a word of its own");
    assert_eq!(cursor("W"), (0, 4));
    assert_eq!(cursor("WW"), (1, 0), "an empty line stops it");
    assert_eq!(cursor("WWW"), (2, 0));
    assert_eq!(cursor("e"), (0, 2));
    assert_eq!(cursor("ee"), (0, 6));
    assert_eq!(cursor("G"), (3, 0));
    assert_eq!(cursor("Gb"), (2, 6));
    assert_eq!(cursor("Gk"), (2, 0));
    assert_eq!(cursor("Ggg"), (0, 0));
    assert_eq!(cursor("3G"), (2, 0));
    assert_eq!(cursor("j"), (1, 0));
    assert_eq!(cursor("<Down>"), (1, 0), "an arrow does what j does");
    assert_eq!(cursor("}"), (1, 0));
    assert_eq!(cursor("}}"), (3, 2), "the last line's end");
    // The column survives a short line.
    assert_eq!(cursor("$jj"), (2, 9));
    // A key bound to nothing does nothing, and drops a count.
    assert_eq!(cursor("3ql"), (0, 1));
}

#[test]
fn vi_changes_text() {
    let text = "one two three\nsecond line\nthird\n";
    let run = |keys: &str| vi_run(text, keys);
    assert_eq!(run("x").0, "ne two three\nsecond line\nthird\n");
    assert_eq!(run("3x").0, " two three\nsecond line\nthird\n");
    assert_eq!(run("wX").0, "onetwo three\nsecond line\nthird\n");
    assert_eq!(run("dw").0, "two three\nsecond line\nthird\n");
    assert_eq!(run("d2w").0, "three\nsecond line\nthird\n");
    assert_eq!(run("2dw").0, "three\nsecond line\nthird\n");
    assert_eq!(run("wwdw").0, "one two \nsecond line\nthird\n", "dw stops at the line end");
    assert_eq!(run("de").0, " two three\nsecond line\nthird\n");
    assert_eq!(run("wD").0, "one \nsecond line\nthird\n");
    assert_eq!(run("d$").0, "\nsecond line\nthird\n");
    assert_eq!(run("dd"), ("second line\nthird\n".into(), (0, 0)));
    assert_eq!(run("2dd").0, "third\n");
    assert_eq!(run("Gdd").0, "one two three\nsecond line\n");
    assert_eq!(run("dj").0, "third\n");
    assert_eq!(run("dG").0, "\n", "the empty line left keeps the file's line end");
    assert_eq!(run("J"), ("one two three second line\nthird\n".into(), (0, 13)));
    // Back out of an operator.
    assert_eq!(run("d<Esc>x").0, "ne two three\nsecond line\nthird\n");
    // Undo and redo.
    assert_eq!(run("ddu").0, text);
    assert_eq!(run("ddu<C-r>").0, "second line\nthird\n");
    assert_eq!(run("xxuu").0, text);
    assert_eq!(run("x2u").0, text);
}

#[test]
fn vi_inserts() {
    let text = "one two\nsecond\n";
    let run = |keys: &str| vi_run(text, keys);
    assert_eq!(run("ihey <Esc>"), ("hey one two\nsecond\n".into(), (0, 3)));
    assert_eq!(run("aX<Esc>").0, "oXne two\nsecond\n");
    assert_eq!(run("wIX<Esc>").0, "Xone two\nsecond\n");
    assert_eq!(run("AX<Esc>"), ("one twoX\nsecond\n".into(), (0, 7)));
    assert_eq!(run("oX<Esc>"), ("one two\nX\nsecond\n".into(), (1, 0)));
    assert_eq!(run("OX<Esc>"), ("X\none two\nsecond\n".into(), (0, 0)));
    assert_eq!(run("iab<BS>c<CR>d<Esc>").0, "ac\ndone two\nsecond\n");
    // In Insert mode keys type, even those Normal mode binds.
    assert_eq!(run("ihjkl<Esc>").0, "hjklone two\nsecond\n");
    assert_eq!(run("cwX<Esc>").0, "X two\nsecond\n", "cw leaves the blank");
    assert_eq!(run("ccX<Esc>").0, "X\nsecond\n");
    assert_eq!(run("wCX<Esc>").0, "one X\nsecond\n");
    assert_eq!(run("sX<Esc>").0, "Xne two\nsecond\n");
    assert_eq!(run("SX<Esc>").0, "X\nsecond\n");
    // A whole insert is one undo, with what began it.
    assert_eq!(run("ihey<CR>there<Esc>u").0, text);
    assert_eq!(run("oX<Esc>u").0, text);
    assert_eq!(run("cwX<Esc>u").0, text);
    assert_eq!(run("cwX<Esc>u<C-r>").0, "X two\nsecond\n");
}

#[test]
fn vi_yanks_and_puts() {
    let text = "one two\nsecond\n";
    let run = |keys: &str| vi_run(text, keys);
    assert_eq!(run("ywP"), ("one one two\nsecond\n".into(), (0, 3)));
    assert_eq!(run("xp"), ("noe two\nsecond\n".into(), (0, 1)));
    assert_eq!(run("yyp"), ("one two\none two\nsecond\n".into(), (1, 0)));
    assert_eq!(run("yyP"), ("one two\none two\nsecond\n".into(), (0, 0)));
    assert_eq!(run("ddp"), ("second\none two\n".into(), (1, 0)));
    assert_eq!(run("Y2p").0, "one two\none two\none two\nsecond\n");
    // A file's last line, without a line break after it.
    assert_eq!(vi_run("a\nb", "jyyp").0, "a\nb\nb");
}

#[test]
fn vi_visual() {
    let text = "one two three\nsecond line\nthird\n";
    let run = |keys: &str| vi_run(text, keys);
    assert_eq!(run("veld"), ("two three\nsecond line\nthird\n".into(), (0, 0)));
    assert_eq!(run("vd").0, "ne two three\nsecond line\nthird\n", "the character under the cursor counts");
    assert_eq!(run("vjd").0, "econd line\nthird\n");
    assert_eq!(run("wvjbd").0, "one econd line\nthird\n", "either way");
    assert_eq!(run("vex").0, " two three\nsecond line\nthird\n");
    assert_eq!(run("veCX<Esc>").0, "X\nsecond line\nthird\n");
    assert_eq!(run("Vd").0, "second line\nthird\n");
    assert_eq!(run("Vjd").0, "third\n");
    assert_eq!(run("GVkd").0, "one two three\n");
    assert_eq!(run("vecX<Esc>").0, "X two three\nsecond line\nthird\n");
    assert_eq!(run("veyP").0, "oneone two three\nsecond line\nthird\n");
    assert_eq!(run("Vyp").0, "one two three\none two three\nsecond line\nthird\n");
    // Yanked text replaces a selection, and takes it in turn.
    assert_eq!(run("ywwvep").0, "one one  three\nsecond line\nthird\n");
    assert_eq!(run("yyjVp").0, "one two three\none two three\nthird\n");
    // Out, keeping the text; the other end; a count.
    assert_eq!(run("ve<Esc>x").0, "on two three\nsecond line\nthird\n");
    assert_eq!(run("vev").1, (0, 2));
    assert_eq!(run("ww").1, (0, 8));
    assert_eq!(run("wwvoh").1, (0, 7));
    assert_eq!(run("v3ld").0, "two three\nsecond line\nthird\n");
    // v to V and back keeps the selection's start.
    assert_eq!(run("jlvVd").0, "one two three\nthird\n");
}

#[test]
fn vi_marks_the_selection_and_clears_it() {
    let mut t = term(40, 20);
    let doc = link(&mut t, "# Title\n\nsome text here\n\nmore\n");
    assert!(t.md_session_start(doc, at(0, 0)));
    let mut vi = crate::vi::Vi::default();
    vi.start(&mut t, doc);
    let bindings = crate::keybind::Bindings::new(&[]);
    vi_keys(&mut t, &mut vi, doc, &bindings, "jjvll");
    let lines = shown_text(&t);
    let y = lines.iter().position(|l| l == "some text here").unwrap();
    let inverse = |t: &Terminal, y: usize| -> Vec<usize> {
        let row = shown(t, y);
        row.cells()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.flags.contains(crate::grid::Flags::INVERSE))
            .map(|(i, _)| i)
            .collect()
    };
    assert_eq!(inverse(&t, y), [0, 1, 2], "the rendered view, its text matched");
    assert_eq!(inverse(&t, y + 1), [0, 1, 2], "and exactly in the source view");
    // Across a block not being edited: its rendered view only.
    vi_keys(&mut t, &mut vi, doc, &bindings, "<Esc>ggjjVjj");
    let lines = shown_text(&t);
    let y = lines.iter().position(|l| l == "some text here").unwrap();
    assert_eq!(inverse(&t, y).len(), 14);
    let y = lines.iter().position(|l| l == "more").unwrap();
    assert_eq!(inverse(&t, y).len(), 4);
    assert_eq!(inverse(&t, y + 1).len(), 4, "with the cursor on it");
    // Leaving Visual mode leaves nothing marked but the cursor.
    vi_keys(&mut t, &mut vi, doc, &bindings, "<Esc>");
    let marked: usize = (0..t.rows()).map(|y| inverse(&t, y).len()).sum();
    assert_eq!(marked, 1);
    t.md_session_end(doc);
    let marked: usize = (0..t.rows()).map(|y| inverse(&t, y).len()).sum();
    assert_eq!(marked, 0);
}

#[test]
fn vi_says_where_it_is_and_what_is_typed() {
    let mut t = term(60, 30);
    let doc = link(&mut t, "one\ntwo\n");
    assert!(t.md_session_start(doc, at(0, 0)));
    let mut vi = crate::vi::Vi::default();
    vi.start(&mut t, doc);
    let bindings = crate::keybind::Bindings::new(&[]);
    assert_eq!(vi.status(), "-- NORMAL --");
    vi_keys(&mut t, &mut vi, doc, &bindings, "2d");
    assert_eq!(vi.status(), "-- NORMAL --  2d");
    vi_keys(&mut t, &mut vi, doc, &bindings, "<Esc>:w");
    assert_eq!(vi.status(), "-- NORMAL --  :w");
    let out = vi_keys(&mut t, &mut vi, doc, &bindings, "<CR>");
    assert_eq!(out, [crate::vi::Outcome::Save]);
    assert_eq!(vi.status(), "-- NORMAL --");
    vi_keys(&mut t, &mut vi, doc, &bindings, "i");
    assert_eq!(vi.status(), "-- INSERT --");
    vi_keys(&mut t, &mut vi, doc, &bindings, "<Esc>V");
    assert_eq!(vi.status(), "-- VISUAL LINE --");
    vi_keys(&mut t, &mut vi, doc, &bindings, "<Esc>");
    let out = vi_keys(&mut t, &mut vi, doc, &bindings, "ZZ");
    assert_eq!(out.last(), Some(&crate::vi::Outcome::SaveAndLeave));
    let out = vi_keys(&mut t, &mut vi, doc, &bindings, ":q!<CR>");
    assert_eq!(out.last(), Some(&crate::vi::Outcome::LeaveDiscarding));
    // Discarding goes back to the file's text.
    vi_keys(&mut t, &mut vi, doc, &bindings, "xxoabc<Esc>dd");
    t.md_session_discard(doc);
    assert!(!t.md_dirty(doc));
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"one\ntwo\n");
}

#[test]
fn vi_keys_come_from_the_table() {
    use crate::input::Mods;
    use crate::keybind::{Action, Binding, Bindings, Modes};
    let mut t = term(60, 30);
    let doc = link(&mut t, "one\ntwo\n");
    assert!(t.md_session_start(doc, at(0, 0)));
    let mut vi = crate::vi::Vi::default();
    vi.start(&mut t, doc);
    let mut extra = Binding::parse(&["n", "j"], Mods::NONE, Modes::NORMAL, Action::MoveDown).unwrap();
    extra.extend(Binding::parse(&["x"], Mods::NONE, Modes::NORMAL, Action::None).unwrap());
    extra.extend(Binding::parse(&["j k"], Mods::NONE, Modes::INSERT, Action::NormalMode).unwrap());
    let bindings = Bindings::new(&extra);
    vi_keys(&mut t, &mut vi, doc, &bindings, "n");
    assert_eq!(t.md_session_cursor(doc), Some(at(1, 0)));
    vi_keys(&mut t, &mut vi, doc, &bindings, "x");
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"one\ntwo\n", "unbound");
    // A key that starts a longer binding, and then one that doesn't finish it.
    vi_keys(&mut t, &mut vi, doc, &bindings, "ijz");
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"one\njztwo\n");
    assert_eq!(vi.status(), "-- INSERT --");
    vi_keys(&mut t, &mut vi, doc, &bindings, "jk");
    assert_eq!(vi.status(), "-- NORMAL --");
    assert_eq!(t.linked_source(doc).unwrap().to_bytes(), b"one\njztwo\n");
}
