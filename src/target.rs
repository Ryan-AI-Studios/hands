//! Actuation targets: `uia:…`, `chr:<u32>`, `g:{col}:{row}`, or physical `{x,y}`.

use crate::chrome;
use crate::error::HandsError;
use crate::space::{Rect, Space};
use crate::uia::{self, ResolvedElement};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    ElementId(String),
    Grid { col: i32, row: i32 },
    Pixel { x: i32, y: i32 },
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub target: Target,
    pub kind: &'static str,
    pub id: Option<String>,
    pub x: i32,
    pub y: i32,
    pub rect: Rect,
    pub hwnd: Option<isize>,
    pub name: String,
    pub role: String,
}

impl Target {
    /// Exactly one of `element_id` / `grid` / (`x` and `y`).
    pub fn parse(
        element_id: Option<&str>,
        grid: Option<&str>,
        x: Option<i32>,
        y: Option<i32>,
    ) -> Result<Self, HandsError> {
        let el = nonempty(element_id);
        let g = nonempty(grid);
        let xy = match (x, y) {
            (Some(px), Some(py)) => Some((px, py)),
            (None, None) => None,
            _ => {
                return Err(HandsError::Target(
                    "pixel target requires both --x and --y".into(),
                ));
            }
        };
        match (el, g, xy) {
            (Some(id), None, None) => parse_element_id(id),
            (None, Some(cell), None) => {
                let (col, row) = Space::parse_cell_id(cell)?;
                Ok(Self::Grid { col, row })
            }
            (None, None, Some((px, py))) => Ok(Self::Pixel { x: px, y: py }),
            (None, None, None) => Err(HandsError::Target(
                "exactly one of --element-id, --grid, or --x/--y is required".into(),
            )),
            _ => Err(HandsError::Target(
                "exactly one of --element-id, --grid, or --x/--y is required".into(),
            )),
        }
    }

    pub fn resolve(&self, space: Space) -> Result<ResolvedTarget, HandsError> {
        match self {
            Self::ElementId(id) => {
                if id.starts_with("chr:") {
                    let hit = chrome::try_resolve(id)?;
                    let found = ResolvedElement {
                        rect: hit.rect,
                        name: hit.name,
                        role: hit.role,
                        hwnd: None,
                    };
                    return finish_element(id, found, space);
                }
                let runtime_id = parse_runtime_id(id)?;
                let found = uia::resolve_runtime_id(&runtime_id)?;
                finish_element(id, found, space)
            }
            Self::Grid { col, row } => {
                let rect = space.cell_rect(*col, *row);
                if rect.area() == 0 || !space.contains(rect) {
                    return Err(HandsError::Target(format!(
                        "grid cell g:{col}:{row} is zero-area or outside the virtual screen"
                    )));
                }
                let (x, y) = rect.center();
                if !space.contains_point(x, y) {
                    return Err(HandsError::Target(format!(
                        "grid cell g:{col}:{row} center is outside the virtual screen"
                    )));
                }
                Ok(ResolvedTarget {
                    target: self.clone(),
                    kind: "grid",
                    id: Some(format!("g:{col}:{row}")),
                    x,
                    y,
                    rect,
                    hwnd: None,
                    name: String::new(),
                    role: String::new(),
                })
            }
            Self::Pixel { x, y } => {
                if !space.contains_point(*x, *y) {
                    return Err(HandsError::Target(format!(
                        "pixel ({x},{y}) is outside the virtual screen"
                    )));
                }
                let rect = Rect {
                    x: *x,
                    y: *y,
                    w: 1,
                    h: 1,
                };
                if rect.area() == 0 {
                    return Err(HandsError::Target("pixel target has zero area".into()));
                }
                Ok(ResolvedTarget {
                    target: self.clone(),
                    kind: "pixel",
                    id: None,
                    x: *x,
                    y: *y,
                    rect,
                    hwnd: None,
                    name: String::new(),
                    role: String::new(),
                })
            }
        }
    }
}

fn finish_element(
    id: &str,
    found: ResolvedElement,
    space: Space,
) -> Result<ResolvedTarget, HandsError> {
    if found.rect.area() == 0 {
        return Err(HandsError::Target(format!(
            "element {id} has zero-area bounding rect"
        )));
    }
    let (x, y) = found.rect.center();
    if !space.contains_point(x, y) {
        return Err(HandsError::Target(format!(
            "element {id} center is outside the virtual screen"
        )));
    }
    Ok(ResolvedTarget {
        target: Target::ElementId(id.to_string()),
        kind: "element",
        id: Some(id.to_string()),
        x,
        y,
        rect: found.rect,
        hwnd: found.hwnd,
        name: found.name,
        role: found.role,
    })
}

pub fn parse_element_id(id: &str) -> Result<Target, HandsError> {
    if let Some(rest) = id.strip_prefix("uia:") {
        if rest.is_empty() {
            return Err(HandsError::Target("element id is a bare uia:".into()));
        }
        let _ = parse_runtime_id(id)?;
        return Ok(Target::ElementId(id.to_string()));
    }
    if id.starts_with("chr:") {
        let n = parse_chr_index(id)?;
        return Ok(Target::ElementId(format!("chr:{n}")));
    }
    if let Some((prefix, _)) = id.split_once(':') {
        return Err(HandsError::Target(format!(
            "unknown element id prefix '{prefix}:'"
        )));
    }
    Err(HandsError::Target(format!(
        "element id must start with uia: or chr: (got '{id}')"
    )))
}

pub fn parse_chr_index(id: &str) -> Result<u32, HandsError> {
    let rest = id.strip_prefix("chr:").ok_or_else(|| {
        HandsError::Target(format!("element id must start with chr: (got '{id}')"))
    })?;
    if rest.is_empty()
        || rest.starts_with('+')
        || rest.starts_with('-')
        || rest.contains('.')
        || rest.as_bytes().iter().any(|b| !b.is_ascii_digit())
        || (rest.len() > 1 && rest.starts_with('0'))
    {
        return Err(HandsError::Target(format!(
            "invalid Chrome element id '{id}' (expected chr:<u32>)"
        )));
    }
    rest.parse::<u32>().map_err(|_| {
        HandsError::Target(format!(
            "invalid Chrome element id '{id}' (expected chr:<u32>)"
        ))
    })
}

pub fn parse_runtime_id(id: &str) -> Result<Vec<i32>, HandsError> {
    let rest = id.strip_prefix("uia:").ok_or_else(|| {
        HandsError::Target(format!("element id must start with uia: (got '{id}')"))
    })?;
    if rest.is_empty() {
        return Err(HandsError::Target("element id is a bare uia:".into()));
    }
    let mut out = Vec::new();
    for part in rest.split('.') {
        if part.is_empty() {
            return Err(HandsError::Target(format!(
                "element id has an empty RuntimeId token (got '{id}')"
            )));
        }
        let n = part.parse::<i32>().map_err(|_| {
            HandsError::Target(format!("element id RuntimeId is not integers (got '{id}')"))
        })?;
        out.push(n);
    }
    if out.is_empty() {
        return Err(HandsError::Target("element id has empty RuntimeId".into()));
    }
    Ok(out)
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_each_kind() {
        assert!(matches!(
            Target::parse(Some("uia:42.1"), None, None, None).unwrap(),
            Target::ElementId(id) if id == "uia:42.1"
        ));
        assert_eq!(
            Target::parse(None, Some("g:-1:2"), None, None).unwrap(),
            Target::Grid { col: -1, row: 2 }
        );
        assert_eq!(
            Target::parse(None, None, Some(10), Some(20)).unwrap(),
            Target::Pixel { x: 10, y: 20 }
        );
    }

    #[test]
    fn chr_grammar_table() {
        for id in ["chr:0", "chr:1", "chr:42"] {
            assert!(
                matches!(parse_element_id(id).unwrap(), Target::ElementId(s) if s == id),
                "{id}"
            );
        }
        for id in [
            "chr:",
            "chr:abc",
            "chr:007",
            "chr: 1",
            "chr:-1",
            "chr:+1",
            "chr:1.0",
            "chr:4294967296",
        ] {
            let err = parse_element_id(id).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("chr:"), "{id}: {msg}");
            assert!(
                !msg.to_ascii_lowercase().contains("unknown prefix"),
                "{id}: {msg}"
            );
        }
        let err = parse_element_id("foo:1").unwrap_err();
        assert!(err.to_string().contains("unknown"), "{err}");
    }

    #[test]
    fn parse_rejects_bare_uia_mixed_and_unknown() {
        let err = Target::parse(Some("uia:"), None, None, None).unwrap_err();
        assert!(err.to_string().contains("bare uia:"), "{err}");
        let err = Target::parse(Some("foo:1"), None, None, None).unwrap_err();
        assert!(err.to_string().contains("unknown"), "{err}");
        assert!(Target::parse(Some("uia:42.1"), Some("g:0:0"), None, None).is_err());
        assert!(Target::parse(Some("chr:0"), Some("g:0:0"), None, None).is_err());
        assert!(Target::parse(None, None, Some(1), None).is_err());
        assert!(Target::parse(None, None, None, None).is_err());
        assert!(Target::parse(Some("42.1"), None, None, None).is_err());
    }

    #[test]
    fn resolve_chr0_from_fixture_center() {
        let g = crate::chrome::EnvGuard::lock();
        g.set_snapshot(Some(&crate::chrome::EnvGuard::fixture_path()));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let hit = Target::ElementId("chr:0".into()).resolve(space).unwrap();
        assert_eq!(hit.kind, "element");
        assert_eq!(hit.id.as_deref(), Some("chr:0"));
        assert_eq!((hit.x, hit.y), (210, 166));
        assert!(hit.hwnd.is_none());
        assert_eq!(hit.role, "Edit");
    }

    #[test]
    fn resolve_chr_errors_missing_malformed_and_no_pipe() {
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let g = crate::chrome::EnvGuard::lock();

        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("chr:") || msg.contains("host") || msg.contains("fixture"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );

        let bad = std::env::temp_dir().join("hands-target-malformed.json");
        std::fs::write(&bad, "not-json").unwrap();
        g.set_snapshot(Some(&bad));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        let _ = std::fs::remove_file(&bad);
        assert!(
            msg.contains("chr:") || msg.contains("host") || msg.contains("fixture"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );

        g.set_snapshot(None);
        g.set_pipe(Some(r"\\.\pipe\hands-chrome-absent-target-0005"));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn resolve_grid_and_pixel_use_area_and_contains() {
        let space = Space::new(0, 0, 250, 180).unwrap();
        let grid = Target::Grid { col: 2, row: 1 };
        let hit = grid.resolve(space).unwrap();
        assert_eq!(hit.kind, "grid");
        assert_eq!((hit.x, hit.y), space.cell_rect(2, 1).center());
        assert!(space.contains(hit.rect));
        assert!(hit.rect.area() > 0);

        let pixel = Target::Pixel { x: 249, y: 179 };
        let hit = pixel.resolve(space).unwrap();
        assert_eq!((hit.x, hit.y), (249, 179));

        assert!(Target::Pixel { x: 250, y: 0 }.resolve(space).is_err());
        assert!(Target::Grid { col: 40, row: 0 }.resolve(space).is_err());
        assert!(Target::Grid { col: -1, row: 0 }.resolve(space).is_err());
        assert!(
            Target::Grid {
                col: i32::MAX,
                row: 0
            }
            .resolve(space)
            .is_err()
        );
    }

    #[test]
    fn stamped_search_box_cell_is_hittable_neighbor_is_not() {
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let rect = Rect {
            x: 46,
            y: 620,
            w: 903,
            h: 56,
        };
        assert_eq!(rect.center(), (497, 648));
        assert_eq!(space.cell_id_of_center(rect), "g:4:6");

        let hit = Target::Grid { col: 4, row: 6 }.resolve(space).unwrap();
        assert_eq!((hit.x, hit.y), (450, 650));
        assert!(hit.x >= rect.x && hit.x < rect.x + rect.w);
        assert!(hit.y >= rect.y && hit.y < rect.y + rect.h);
        assert_eq!((hit.x, hit.y), space.cell_rect(4, 6).center());

        let miss = Target::Grid { col: 2, row: 5 }.resolve(space).unwrap();
        assert_eq!((miss.x, miss.y), (250, 550));
        let inside = miss.x >= rect.x
            && miss.x < rect.x + rect.w
            && miss.y >= rect.y
            && miss.y < rect.y + rect.h;
        assert!(!inside);
    }
}
