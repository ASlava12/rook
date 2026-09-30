use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Match configuration spelling without confusing printable Shift input with
/// an application shortcut. Unbound text still goes through the editor.
pub(super) fn name(event: KeyEvent) -> String {
    if !event
        .modifiers
        .difference(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER)
        .is_empty()
    {
        return String::new();
    }
    let key = match event.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char('+') => "plus".into(),
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "escape".into(),
        KeyCode::Tab | KeyCode::BackTab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        _ => return String::new(),
    };
    let mut modifiers = event.modifiers;
    if event.code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    let mut name = String::new();
    for (modifier, label) in [
        (KeyModifiers::CONTROL, "ctrl+"),
        (KeyModifiers::ALT, "alt+"),
        (KeyModifiers::SHIFT, "shift+"),
        (KeyModifiers::SUPER, "super+"),
    ] {
        if modifiers.contains(modifier) {
            name.push_str(label);
        }
    }
    name.push_str(&key);
    name
}

pub(super) fn hint(
    bindings: &rook_core::keybindings::Bindings,
    action: rook_core::keybindings::Action,
) -> String {
    let label = bindings.label(action);
    let first = label.split(" / ").next().unwrap_or("unbound");
    let display = first.replace("ctrl+", "^").replace("alt+", "Alt+");
    if first.starts_with('f') && first.len() <= 3 { display.to_ascii_uppercase() } else { display }
}
