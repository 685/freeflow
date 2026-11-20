use super::*;
use base64::Engine as _;
use yaml_edit::{Mapping, Sequence, YamlFile, YamlNode};

pub const MAX_ASSET: usize = 10 * 1024 * 1024;

fn version(metadata: &fs::Metadata) -> Result<String> {
    Ok(files::revision(format!(
        "{}:{:?}",
        metadata.len(),
        metadata.modified()?
    )))
}

pub fn kind(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "ico" | "bmp" | "wbmp" | "svg" => {
            Some("images")
        }
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" | "opus" => Some("audio"),
        "mp4" | "m4v" | "webm" | "mov" | "ogv" => Some("videos"),
        "ttf" | "otf" => Some("fonts"),
        "pdf" | "txt" | "json" | "csv" | "xml" | "md" | "arb" | "zip" => Some("documents"),
        _ => None,
    }
}

fn string(node: &YamlNode) -> Option<String> {
    node.as_scalar().map(|scalar| scalar.as_string())
}

fn asset_path(node: &YamlNode) -> Option<String> {
    string(node).or_else(|| node.as_mapping()?.get("path").as_ref().and_then(string))
}

fn sequence(mapping: &Mapping, name: &str) -> Result<Sequence> {
    if mapping
        .get(name)
        .is_none_or(|node| node.as_scalar().is_some_and(|scalar| scalar.is_null()))
    {
        mapping.set(name, Sequence::new());
    }
    mapping.get_sequence(name).ok_or_else(|| {
        Problem::new(
            422,
            format!(
                "flutter.{name} must be a YAML list. Edit pubspec.yaml before uploading assets."
            ),
        )
    })
}

struct Manifest {
    yaml: YamlFile,
    flutter: Mapping,
    crlf: bool,
}

impl Manifest {
    fn parse(source: &str, create: bool) -> Result<Self> {
        let yaml: YamlFile = source.parse().map_err(|error| {
            Problem::new(
                422,
                format!("Fix pubspec.yaml before changing assets: {error}"),
            )
        })?;
        let documents: Vec<_> = yaml.documents().collect();
        let root = (documents.len() == 1)
            .then(|| documents[0].as_mapping())
            .flatten()
            .ok_or_else(|| Problem::new(422, "pubspec.yaml must contain one YAML mapping."))?;
        for mapping in [
            &root,
            &root.get_mapping("flutter").unwrap_or_else(Mapping::new),
        ] {
            let mut keys = HashSet::new();
            if mapping
                .keys()
                .filter_map(|node| string(&node))
                .any(|key| !keys.insert(key))
            {
                return Err(Problem::new(
                    422,
                    "Remove duplicate YAML keys from pubspec.yaml before changing assets.",
                ));
            }
        }
        if root
            .get("flutter")
            .is_none_or(|node| node.as_scalar().is_some_and(|scalar| scalar.is_null()))
            && create
        {
            root.set("flutter", Mapping::new());
        }
        let flutter = match root.get_mapping("flutter") {
            Some(mapping) => mapping,
            None if !create
                && root
                    .get("flutter")
                    .is_none_or(|node| node.as_scalar().is_some_and(|scalar| scalar.is_null())) =>
            {
                Mapping::new()
            }
            None => {
                return Err(Problem::new(
                    422,
                    "The flutter section must be a YAML mapping. Edit pubspec.yaml before changing assets.",
                ));
            }
        };
        Ok(Self {
            yaml,
            flutter,
            crlf: source.contains("\r\n"),
        })
    }

    fn family(&self, path: &str) -> Option<String> {
        self.flutter
            .get_sequence("fonts")?
            .values()
            .find_map(|item| {
                let group = item.as_mapping()?;
                let fonts = group.get_sequence("fonts")?;
                fonts
                    .values()
                    .any(|font| {
                        font.as_mapping()
                            .and_then(|font| font.get("asset"))
                            .as_ref()
                            .and_then(string)
                            .as_deref()
                            == Some(path)
                    })
                    .then(|| group.get("family").as_ref().and_then(string))
                    .flatten()
            })
    }

    fn registered(&self, path: &str) -> bool {
        self.family(path).is_some()
            || self.flutter.get_sequence("assets").is_some_and(|assets| {
                assets
                    .values()
                    .filter_map(|item| asset_path(&item))
                    .any(|entry| {
                        entry == path
                            || entry.ends_with('/')
                                && path
                                    .strip_prefix(&entry)
                                    .is_some_and(|rest| !rest.contains('/'))
                    })
            })
    }

    fn register(&self, path: &str) -> Result<()> {
        if kind(path) == Some("fonts") && self.family(path).is_none() {
            let stem = path.rsplit('/').next().unwrap().rsplit_once('.').unwrap().0;
            let groups = sequence(&self.flutter, "fonts")?;
            let names: HashSet<_> = groups
                .values()
                .filter_map(|group| group.as_mapping()?.get("family").as_ref().and_then(string))
                .collect();
            let mut family = stem.to_owned();
            let mut suffix = 2;
            while names.contains(&family) {
                family = format!("{stem}_{suffix}");
                suffix += 1;
            }
            let font = Mapping::new();
            font.set("asset", path);
            let variants = Sequence::new();
            variants.push(font);
            let group = Mapping::new();
            group.set("family", family.as_str());
            group.set("fonts", variants);
            groups.push(group);
        } else if !self.registered(path) {
            sequence(&self.flutter, "assets")?.push(path);
        }
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<()> {
        if self.flutter.get("assets").is_some() {
            let assets = sequence(&self.flutter, "assets")?;
            for index in (0..assets.len()).rev() {
                if assets.get(index).as_ref().and_then(asset_path).as_deref() == Some(path) {
                    assets.remove(index);
                }
            }
        }
        if self.flutter.get("fonts").is_some() {
            let groups = sequence(&self.flutter, "fonts")?;
            for index in (0..groups.len()).rev() {
                let Some(group) = groups
                    .get(index)
                    .and_then(|node| node.as_mapping().cloned())
                else {
                    continue;
                };
                let Some(fonts) = group.get_sequence("fonts") else {
                    continue;
                };
                let mut changed = false;
                for font in (0..fonts.len()).rev() {
                    if fonts
                        .get(font)
                        .and_then(|node| node.as_mapping()?.get("asset"))
                        .as_ref()
                        .and_then(string)
                        .as_deref()
                        == Some(path)
                    {
                        fonts.remove(font);
                        changed = true;
                    }
                }
                if changed && fonts.is_empty() {
                    groups.remove(index);
                }
            }
        }
        Ok(())
    }

    fn render(&self) -> Result<String> {
        let content = self.yaml.to_string();
        let content = if self.crlf {
            content.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            content
        };
        if content.len() > files::MAX_TEXT {
            return Err(Problem::new(413, "pubspec.yaml is too large to update."));
        }
        let _: YamlFile = content.parse().map_err(|error| {
            Problem::new(
                422,
                format!("The proposed asset declaration is invalid: {error}"),
            )
        })?;
        Ok(content)
    }
}

impl Projects {
    pub fn asset_context(&self, project: &Project, input: &Value) -> Result<Value> {
        let catalog = self.assets(project)?;
        let query = input["query"].as_str().unwrap_or_default().to_lowercase();
        let category = input["category"].as_str().unwrap_or_default();
        let filtered: Vec<_> = catalog["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                (category.is_empty() || entry["category"] == category)
                    && format!(
                        "{} {}",
                        entry["path"].as_str().unwrap_or_default(),
                        entry["family"].as_str().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        let mut entries = vec![];
        let mut bytes = 0;
        for entry in filtered.iter().take(200) {
            bytes += serde_json::to_vec(entry)?.len();
            if bytes > 96 * 1024 {
                break;
            }
            entries.push(*entry);
        }
        Ok(json!({"entries":entries,
            "truncated":entries.len()<filtered.len() || catalog["truncated"] == true,
            "manifestRevision":catalog["manifestRevision"],"manifestError":catalog["manifestError"]}))
    }

    pub fn assets(&self, project: &Project) -> Result<Value> {
        let _guard = self.writes.lock().unwrap();
        let source = self.file(project, "pubspec.yaml")?;
        let manifest = Manifest::parse(&source.content, false);
        let tree = self.tree(project)?;
        let mut entries = vec![];
        for entry in tree.entries.iter().filter(|entry| entry.kind == "file") {
            let Some(category) = kind(&entry.path) else {
                continue;
            };
            if !entry.path.starts_with("assets/")
                && !manifest
                    .as_ref()
                    .is_ok_and(|manifest| manifest.registered(&entry.path))
            {
                continue;
            }
            let target = files::project_file(&project.root, &entry.path, false)?;
            let metadata = fs::metadata(&target)?;
            let size = metadata.len();
            let family = manifest
                .as_ref()
                .ok()
                .and_then(|manifest| manifest.family(&entry.path));
            entries.push(json!({"path":entry.path,"category":category,"size":size,"family":family,
                "registered":manifest.as_ref().is_ok_and(|manifest| manifest.registered(&entry.path)),
                "revision":if size <= MAX_ASSET as u64 {Some(version(&metadata)?)} else {None}}));
        }
        entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        Ok(
            json!({"entries":entries,"truncated":tree.truncated,"manifestRevision":source.revision,
            "manifestError":manifest.err().map(|error| error.message),"maxBytes":MAX_ASSET}),
        )
    }

    pub fn asset(&self, project: &Project, name: &str, encoded: &str) -> Result<Value> {
        let category = kind(name).ok_or_else(|| {
            Problem::new(
                415,
                "Choose an image, audio, video, TTF/OTF font or document.",
            )
        })?;
        if name.len() > 180 || files::relative_parts(name)?.len() != 1 {
            return Err(Problem::new(
                400,
                "Choose a filename without folders, up to 180 bytes.",
            ));
        }
        if encoded.len() > MAX_ASSET.div_ceil(3) * 4 {
            return Err(Problem::new(413, "Assets must be 10 MiB or smaller."));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
            .map_err(|_| Problem::new(400, "Invalid asset data."))?;
        if bytes.is_empty() || bytes.len() > MAX_ASSET {
            return Err(Problem::new(
                413,
                "Assets must be between 1 byte and 10 MiB.",
            ));
        }
        let relative = format!("assets/{category}/{name}");
        let mut journal = self.writes.lock().unwrap();
        let target = files::project_file(&project.root, &relative, true)?;
        if files::exists(&target)? {
            return Err(Problem::new(
                409,
                "An asset with that name already exists. Rename the file before uploading.",
            ));
        }
        let source = self
            .agent_source(project, "pubspec.yaml")?
            .ok_or_else(|| Problem::new(404, "pubspec.yaml is missing."))?;
        let manifest = Manifest::parse(&source.content, true)?;
        manifest.register(&relative)?;
        let next = manifest.render()?;
        let mut changes = vec![ChangedFile {
            path: relative.clone(),
            before: None,
            after: Some(bytes),
            mode: 0o644,
        }];
        if next != source.content {
            changes.push(ChangedFile {
                path: "pubspec.yaml".into(),
                before: Some(source.content.into_bytes()),
                after: Some(next.into_bytes()),
                mode: file_mode(&project.root.join("pubspec.yaml"), true)?,
            });
        }
        self.apply_changes_inner(project, &changes)?;
        journal.record(&project.id, changes);
        Ok(
            json!({"path":relative,"category":category,"revision":version(&fs::metadata(&target)?)?,"family":manifest.family(&relative),"registered":true}),
        )
    }

    pub fn delete_asset(
        &self,
        project: &Project,
        relative: &str,
        revision: &str,
        manifest_revision: &str,
    ) -> Result<Value> {
        self.change_asset(project, relative, revision, manifest_revision, true)
    }

    pub fn register_asset(
        &self,
        project: &Project,
        relative: &str,
        revision: &str,
        manifest_revision: &str,
    ) -> Result<Value> {
        self.change_asset(project, relative, revision, manifest_revision, false)
    }

    fn change_asset(
        &self,
        project: &Project,
        relative: &str,
        revision: &str,
        manifest_revision: &str,
        remove: bool,
    ) -> Result<Value> {
        if kind(relative).is_none() {
            return Err(Problem::new(415, "Choose an app asset."));
        }
        let mut journal = self.writes.lock().unwrap();
        let target = files::project_file(&project.root, relative, false)?;
        let metadata = fs::metadata(&target)?;
        if metadata.len() > MAX_ASSET as u64 {
            return Err(Problem::new(413, "Asset changes are limited to 10 MiB."));
        }
        let bytes = fs::read(&target)?;
        let source = self
            .agent_source(project, "pubspec.yaml")?
            .ok_or_else(|| Problem::new(404, "pubspec.yaml is missing."))?;
        if source.revision.as_deref() != Some(manifest_revision)
            || version(&metadata)? != revision
            || version(&fs::metadata(&target)?)? != revision
        {
            return Err(Problem::new(
                409,
                "This asset or pubspec.yaml changed. Refresh Assets before continuing.",
            ));
        }
        let manifest = Manifest::parse(&source.content, !remove)?;
        if !relative.starts_with("assets/") && !manifest.registered(relative) {
            return Err(Problem::new(
                403,
                "Only files in Assets or declared Flutter assets can be changed here.",
            ));
        }
        if remove {
            manifest.remove(relative)?;
        } else {
            manifest.register(relative)?;
        }
        let next = manifest.render()?;
        let mut changes = if remove {
            vec![ChangedFile {
                path: relative.into(),
                before: Some(bytes),
                after: None,
                mode: file_mode(&target, true)?,
            }]
        } else {
            vec![]
        };
        if next != source.content {
            changes.push(ChangedFile {
                path: "pubspec.yaml".into(),
                before: Some(source.content.into_bytes()),
                after: Some(next.into_bytes()),
                mode: file_mode(&project.root.join("pubspec.yaml"), true)?,
            });
        }
        self.apply_changes_inner(project, &changes)?;
        journal.record(&project.id, changes);
        Ok(
            json!({"path":relative,"removed":remove,"registered":!remove,"family":manifest.family(relative)}),
        )
    }
}
