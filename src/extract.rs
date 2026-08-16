use serde::Serialize;

use crate::space::Rect;

pub const TITLE_MAX_CHARS: usize = 200;
pub const MAIN_TEXT_MAX_CHARS: usize = 1500;
pub const DEFAULT_ELEMENT_CAP: usize = 250;
pub const DOM_ELEMENT_CAP: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Default,
    Dom,
}

impl Detail {
    pub fn element_cap(self) -> usize {
        match self {
            Self::Default => DEFAULT_ELEMENT_CAP,
            Self::Dom => DOM_ELEMENT_CAP,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dom => "dom",
        }
    }

    pub fn parse_arg(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::Default),
            Some(s) if s.eq_ignore_ascii_case("default") => Ok(Self::Default),
            Some(s) if s.eq_ignore_ascii_case("dom") => Ok(Self::Dom),
            Some(other) => Err(format!(
                "unknown detail '{other}' (expected default or dom)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Button,
    Edit,
    Document,
    Hyperlink,
    ComboBox,
    ListItem,
    MenuItem,
    CheckBox,
    RadioButton,
    TabItem,
    TreeItem,
    Slider,
    SplitButton,
    Text,
    Other,
}

impl ControlKind {
    pub fn from_uia_id(id: i32) -> Self {
        match id {
            50_000 => Self::Button,
            50_002 => Self::CheckBox,
            50_003 => Self::ComboBox,
            50_004 => Self::Edit,
            50_005 => Self::Hyperlink,
            50_007 => Self::ListItem,
            50_011 => Self::MenuItem,
            50_013 => Self::RadioButton,
            50_015 => Self::Slider,
            50_019 => Self::TabItem,
            50_020 => Self::Text,
            50_024 => Self::TreeItem,
            50_030 => Self::Document,
            50_031 => Self::SplitButton,
            _ => Self::Other,
        }
    }

    pub fn is_hittable(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Edit
                | Self::Document
                | Self::Hyperlink
                | Self::ComboBox
                | Self::ListItem
                | Self::MenuItem
                | Self::CheckBox
                | Self::RadioButton
                | Self::TabItem
                | Self::TreeItem
                | Self::Slider
                | Self::SplitButton
        )
    }

    pub fn contributes_main_text(self) -> bool {
        matches!(self, Self::Document | Self::Edit | Self::Text)
    }

    pub fn type_name(self) -> &'static str {
        match self {
            Self::Button => "Button",
            Self::Edit => "Edit",
            Self::Document => "Document",
            Self::Hyperlink => "Hyperlink",
            Self::ComboBox => "ComboBox",
            Self::ListItem => "ListItem",
            Self::MenuItem => "MenuItem",
            Self::CheckBox => "CheckBox",
            Self::RadioButton => "RadioButton",
            Self::TabItem => "TabItem",
            Self::TreeItem => "TreeItem",
            Self::Slider => "Slider",
            Self::SplitButton => "SplitButton",
            Self::Text => "Text",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RawNode {
    pub runtime_id: Vec<i32>,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub is_password: bool,
    pub rect: Rect,
    pub control_kind: ControlKind,
    pub is_control: bool,
    pub is_offscreen: bool,
    pub is_keyboard_focusable: bool,
}

impl RawNode {
    pub fn element_id(&self) -> Option<String> {
        if self.runtime_id.is_empty() {
            return None;
        }
        Some(format!(
            "uia:{}",
            self.runtime_id
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        ))
    }

    pub fn passes_filter(&self, detail: Detail) -> bool {
        if self.element_id().is_none() {
            return false;
        }
        if !self.is_control || self.is_offscreen || self.rect.w <= 0 || self.rect.h <= 0 {
            return false;
        }
        match detail {
            Detail::Dom => true,
            Detail::Default => self.is_keyboard_focusable || self.control_kind.is_hittable(),
        }
    }

    pub fn to_element(&self) -> Option<Element> {
        Some(Element {
            id: self.element_id()?,
            role: self.role.clone(),
            text: if self.is_password {
                None
            } else {
                Some(self.name.clone())
            },
            rect: self.rect,
        })
    }

    pub fn main_text_piece(&self) -> Option<String> {
        if self.is_password || !self.control_kind.contributes_main_text() {
            return None;
        }
        let mut out = String::new();
        if !self.name.is_empty() {
            out.push_str(&self.name);
        }
        if let Some(value) = &self.value
            && !value.is_empty()
            && value != &self.name
        {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(value);
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Element {
    pub id: String,
    pub role: String,
    pub text: Option<String>,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extract {
    pub title: String,
    pub url: Option<String>,
    pub main_text: String,
    pub cards: Vec<serde_json::Value>,
}

pub fn filter_nodes(nodes: &[RawNode], detail: Detail) -> (Vec<Element>, usize) {
    let cap = detail.element_cap();
    let mut elements = Vec::new();
    let mut matched = 0usize;
    for node in nodes {
        if !node.passes_filter(detail) {
            continue;
        }
        matched += 1;
        if elements.len() < cap
            && let Some(element) = node.to_element()
        {
            elements.push(element);
        }
    }
    (elements, matched)
}

pub fn extract_from_nodes(title: &str, nodes: &[RawNode]) -> Extract {
    let mut main_text = String::new();
    for node in nodes {
        if let Some(piece) = node.main_text_piece() {
            main_text.push_str(&piece);
            if main_text.chars().count() >= MAIN_TEXT_MAX_CHARS {
                break;
            }
        }
    }
    Extract {
        title: take_chars(title, TITLE_MAX_CHARS),
        url: None,
        main_text: take_chars(&main_text, MAIN_TEXT_MAX_CHARS),
        cards: Vec::new(),
    }
}

pub fn take_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: ControlKind, name: &str) -> RawNode {
        RawNode {
            runtime_id: vec![42, 1],
            role: kind.type_name().to_string(),
            name: name.to_string(),
            value: None,
            is_password: false,
            rect: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            control_kind: kind,
            is_control: true,
            is_offscreen: false,
            is_keyboard_focusable: false,
        }
    }

    #[test]
    fn default_filter_requires_hittable_or_focusable() {
        let button = node(ControlKind::Button, "OK");
        let text = node(ControlKind::Text, "label");
        let mut focus_text = text.clone();
        focus_text.is_keyboard_focusable = true;
        assert!(button.passes_filter(Detail::Default));
        assert!(!text.passes_filter(Detail::Default));
        assert!(focus_text.passes_filter(Detail::Default));
        assert!(text.passes_filter(Detail::Dom));
    }

    #[test]
    fn filter_skips_offscreen_and_zero_size() {
        let mut off = node(ControlKind::Button, "x");
        off.is_offscreen = true;
        let mut zero = node(ControlKind::Button, "x");
        zero.rect.w = 0;
        let mut not_control = node(ControlKind::Button, "x");
        not_control.is_control = false;
        assert!(!off.passes_filter(Detail::Default));
        assert!(!zero.passes_filter(Detail::Dom));
        assert!(!not_control.passes_filter(Detail::Dom));
    }

    #[test]
    fn default_cap_250_and_dom_cap_2000() {
        let nodes: Vec<RawNode> = (0..300)
            .map(|i| {
                let mut n = node(ControlKind::Button, "b");
                n.runtime_id = vec![1, i];
                n
            })
            .collect();
        let (els, matched) = filter_nodes(&nodes, Detail::Default);
        assert_eq!(matched, 300);
        assert_eq!(els.len(), 250);
        assert_eq!(els[0].id, "uia:1.0");
        assert_eq!(els[249].id, "uia:1.249");

        let many: Vec<RawNode> = (0..2_100)
            .map(|i| {
                let mut n = node(ControlKind::Text, "t");
                n.runtime_id = vec![2, i];
                n
            })
            .collect();
        let (els, matched) = filter_nodes(&many, Detail::Dom);
        assert_eq!(matched, 2_100);
        assert_eq!(els.len(), 2_000);
    }

    #[test]
    fn extract_caps_and_skips_password() {
        let title = "T".repeat(250);
        let mut doc = node(ControlKind::Document, &"A".repeat(800));
        doc.value = Some("B".repeat(800));
        let mut password = node(ControlKind::Edit, "secret");
        password.is_password = true;
        password.value = Some("hunter2".into());
        let extract = extract_from_nodes(&title, &[password, doc]);
        assert_eq!(extract.title.chars().count(), 200);
        assert_eq!(extract.url, None);
        assert!(extract.cards.is_empty());
        assert_eq!(extract.main_text.chars().count(), 1500);
        assert!(!extract.main_text.contains("secret"));
        assert!(!extract.main_text.contains("hunter2"));
        assert!(extract.main_text.starts_with('A'));
    }

    #[test]
    fn password_element_text_is_null() {
        let mut password = node(ControlKind::Edit, "secret");
        password.is_password = true;
        let el = password.to_element().expect("valid runtime id");
        assert_eq!(el.text, None);
        assert_eq!(el.id, "uia:42.1");
    }

    #[test]
    fn empty_runtime_id_is_skipped() {
        let mut n = node(ControlKind::Button, "OK");
        n.runtime_id.clear();
        assert!(!n.passes_filter(Detail::Default));
        assert!(!n.passes_filter(Detail::Dom));
        assert!(n.to_element().is_none());
        let (els, matched) = filter_nodes(&[n], Detail::Default);
        assert_eq!(matched, 0);
        assert!(els.is_empty());
    }
}
