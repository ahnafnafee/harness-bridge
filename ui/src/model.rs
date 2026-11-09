use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Deserialize)]
pub struct Session {
    pub provider: String,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub created_ms: Option<i64>,
    pub updated_ms: Option<i64>,
}

impl Session {
    pub fn title(&self) -> &str {
        self.title
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled session")
    }
    pub fn key(&self) -> String {
        format!("{}:{}", self.provider, self.id)
    }
    pub fn time(&self) -> Option<i64> {
        self.updated_ms.or(self.created_ms)
    }
}

#[derive(Clone, Default, Deserialize)]
pub struct Provider {
    pub name: String,
    pub label: String,
    pub writable: bool,
    pub note: String,
}

#[derive(Clone, Default, Deserialize)]
pub struct Source {
    pub info: Provider,
    pub sessions: usize,
    pub problem: Option<String>,
}

#[derive(Default, Deserialize)]
pub struct Catalog {
    pub sessions: Vec<Session>,
    pub providers: Vec<Source>,
}

#[derive(Default, Deserialize)]
pub struct CatalogReply {
    pub catalog: Catalog,
    pub providers: Vec<Provider>,
    pub settings: Value,
    pub version: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Activity {
    pub title: String,
    pub from: String,
    pub to: String,
    pub location: String,
    pub date: f64,
}

pub fn project(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(if path.is_empty() {
            "No project folder"
        } else {
            path
        })
}

pub fn filtered(
    sessions: &[Session],
    source: &str,
    folder: Option<&str>,
    query: &str,
    sort: &str,
    providers: &[Provider],
) -> Vec<Session> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut rows: Vec<_> = sessions
        .iter()
        .filter(|s| {
            let label = providers
                .iter()
                .find(|p| p.name == s.provider)
                .map(|p| p.label.as_str())
                .unwrap_or(&s.provider);
            let text = format!(
                "{} {} {} {} {}",
                s.title(),
                s.id,
                s.cwd.as_deref().unwrap_or_default(),
                s.provider,
                label
            )
            .to_lowercase();
            (source.is_empty() || source == s.provider)
                && folder.is_none_or(|f| f == s.cwd.as_deref().unwrap_or_default())
                && terms.iter().all(|q| text.contains(q))
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        let order = match sort {
            "Oldest first" => a.time().cmp(&b.time()),
            "Title A–Z" => a.title().to_lowercase().cmp(&b.title().to_lowercase()),
            _ => b.time().cmp(&a.time()),
        };
        order
            .then(a.provider.cmp(&b.provider))
            .then(a.id.cmp(&b.id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str, provider: &str, folder: Option<&str>, time: i64) -> Session {
        Session {
            id: id.into(),
            provider: provider.into(),
            title: Some("Résumé context".into()),
            cwd: folder.map(str::to_owned),
            created_ms: Some(time),
            ..Default::default()
        }
    }
    #[test]
    fn search_intersects_labels_folders_and_unicode() {
        let rows = vec![
            session("1", "claude", Some("F:\\work\\bridge"), 1),
            session("2", "qoder", Some("/work/bridge"), 2),
        ];
        let providers = vec![Provider {
            name: "claude".into(),
            label: "Claude Code".into(),
            ..Default::default()
        }];
        assert_eq!(
            filtered(
                &rows,
                "",
                None,
                "résumé code bridge",
                "Newest first",
                &providers
            )[0]
            .id,
            "1"
        );
        assert!(filtered(&rows, "qoder", Some("F:\\work\\bridge"), "", "", &providers).is_empty());
        assert_eq!(rows.len(), 2);
    }
    #[test]
    fn no_folder_differs_from_all_folders_and_identity_includes_provider() {
        let rows = vec![
            session("same", "claude", None, 1),
            session("same", "qoder", Some("/work"), 2),
        ];
        assert_eq!(filtered(&rows, "", Some(""), "", "", &[]).len(), 1);
        assert_ne!(rows[0].key(), rows[1].key());
    }
    #[test]
    fn sorting_uses_creation_fallback_and_stable_ties() {
        let rows = vec![
            session("b", "claude", None, 1),
            session("a", "claude", None, 1),
            session("c", "qoder", None, 2),
        ];
        assert_eq!(
            filtered(&rows, "", None, "", "Newest first", &[])
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
        assert_eq!(
            filtered(&rows, "", None, "", "Oldest first", &[])[0].id,
            "a"
        );
    }
    #[test]
    fn project_names_support_both_platforms() {
        assert_eq!(project("F:\\work\\bridge\\"), "bridge");
        assert_eq!(project("/work/bridge/"), "bridge");
        assert_eq!(project(""), "No project folder");
        assert_eq!(project("/"), "/");
    }
}
