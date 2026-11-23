use super::*;
use crate::visual::library::{
    Constructor,
    switching::{Call, Destination, ValueSource},
};

struct Selection<'a> {
    call: Call<'a>,
    scope: visual::Scope,
    definition: PathBuf,
    revision: String,
    project_path: Option<String>,
    alternatives: Vec<Constructor>,
}

fn load_definition(
    projects: &Projects,
    project: &Project,
    path: &Path,
) -> Option<(files::Source, Option<String>)> {
    if let Ok(relative) = path.strip_prefix(&project.root) {
        let relative = relative.to_str()?.replace('\\', "/");
        let source = projects.file(project, &relative).ok()?;
        return Some((source, Some(relative)));
    }
    // Dart can navigate to any file it knows about. Offer definitions from
    // the project, the selected Flutter SDK and the library's trusted packages.
    package_sources(projects, Some(project))
        .0
        .iter()
        .find_map(|package| {
            let relative = path
                .strip_prefix(dunce::canonicalize(&package.root).ok()?)
                .ok()?;
            let relative = relative.to_str()?.replace('\\', "/");
            let file = normalized(&package.root, &relative)?;
            Some((files::read_source(&file, &relative).ok()?, None))
        })
}

async fn selection<'a>(
    jobs: &Jobs,
    project: &Project,
    file: &'a files::Source,
    id: &'a str,
) -> Result<Selection<'a>> {
    let scope = visual::Scope::for_source(&jobs.projects, project, &file.path, &file.content);
    let call = Call::locate(&file.content, id, &scope)?;
    let definitions = jobs
        .analyzers
        .saved_definitions(jobs, project, &file.path, &file.content, call.symbol)
        .await?;
    for definition in definitions {
        let Some((source, project_path)) =
            load_definition(&jobs.projects, project, &definition.file)
        else {
            continue;
        };
        let unit = visual::library::unit(&source.content)?;
        let Some(class) = unit
            .classes
            .into_iter()
            .find(|class| class.name == call.class)
        else {
            continue;
        };
        let resolved = class.constructors.iter().any(|constructor| {
            let name = if constructor.name == "new" {
                ""
            } else {
                &constructor.name
            };
            name == call.constructor && definition.points_to(&source.content, constructor.symbol)
        });
        if !(resolved
            || call.constructor.is_empty() && definition.points_to(&source.content, class.symbol))
        {
            continue;
        }
        let same_file = project_path.as_deref() == Some(&file.path);
        let mut alternatives = vec![];
        for mut constructor in class.constructors {
            if constructor.name == "new" {
                constructor.name.clear();
            }
            if constructor.name != call.constructor
                && (same_file || !constructor.name.starts_with('_'))
                && (!class.abstract_class || constructor.factory)
            {
                alternatives.push(constructor);
            }
        }
        alternatives.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(Selection {
            call,
            scope,
            definition: definition.file,
            revision: source.revision.unwrap_or_default(),
            project_path,
            alternatives,
        });
    }
    Err(Problem::new(
        422,
        "Dart could not resolve the selected widget to a supported constructor.",
    ))
}

fn display_name(class: &str, constructor: &str) -> String {
    if constructor.is_empty() {
        class.into()
    } else {
        format!("{class}.{constructor}")
    }
}

fn entry(selection: &Selection<'_>, constructor: &Constructor) -> Value {
    json!({
        "name": display_name(selection.call.class, &constructor.name),
        "constructor": constructor.name,
        "revision": selection.revision,
        "projectPath": selection.project_path,
        "constant": constructor.constant
    })
}

pub(crate) async fn alternatives(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let file = source(&jobs.projects, project, input)?;
    let selection = selection(jobs, project, &file, required(input, "id")?).await?;
    source(&jobs.projects, project, input)?;
    current_definition(&selection.definition, &selection.revision)?;
    let entries: Vec<_> = selection
        .alternatives
        .iter()
        .map(|constructor| entry(&selection, constructor))
        .collect();
    Ok(json!({
        "current": display_name(selection.call.class, selection.call.constructor),
        "entries": entries
    }))
}

struct Existing<'a> {
    parameter: &'a Parameter,
    index: usize,
}

fn existing<'a>(call: &Call<'_>, parameters: &'a [Parameter]) -> Result<Vec<Existing<'a>>> {
    let mut positional = parameters.iter().filter(|parameter| !parameter.named);
    call.arguments.iter().enumerate().map(|(index, argument)| {
        let parameter = if let Some(name) = argument.name {
            parameters.iter().find(|parameter| parameter.named && parameter.name == name)
        } else {
            positional.next()
        };
        parameter.map(|parameter| Existing { parameter, index }).ok_or_else(|| {
            Problem::new(422, "Fix the widget's current arguments in Code before changing its constructor.")
        })
    }).collect()
}

struct Options<'a> {
    selection: Selection<'a>,
    constructor: Constructor,
    previous: Vec<Parameter>,
    parameters: Vec<Parameter>,
    signature: Value,
}

impl Options<'_> {
    fn revision(&self) -> Result<String> {
        Ok(files::revision(serde_json::to_vec(&(
            &self.previous,
            &self.parameters,
        ))?))
    }
}

async fn options<'a>(
    jobs: &Jobs,
    project: &Project,
    file: &'a files::Source,
    input: &'a Value,
) -> Result<Options<'a>> {
    let selection = selection(jobs, project, file, required(input, "id")?).await?;
    if input["entryRevision"] != selection.revision {
        return Err(Problem::new(
            409,
            "The widget definition changed. Choose the constructor again.",
        ));
    }
    let constructor = selection
        .alternatives
        .iter()
        .find(|constructor| input["constructor"] == constructor.name)
        .ok_or_else(|| Problem::new(422, "Choose another constructor of the selected widget."))?
        .clone();
    let (text, offset) = selection.call.signature_probe(None);
    let previous = signature(jobs, project, &file.path, &text, offset).await?;
    let previous = visual::library::parameters(&previous)?;
    let (text, offset) = selection.call.signature_probe(Some(&constructor.name));
    let signature = signature(jobs, project, &file.path, &text, offset).await?;
    let parameters = visual::library::parameters(&signature)?;
    source(&jobs.projects, project, input)?;
    current_definition(&selection.definition, &selection.revision)?;
    Ok(Options {
        selection,
        constructor,
        previous,
        parameters,
        signature,
    })
}

fn initial_argument(parameter: &Parameter, existing: &[Existing<'_>]) -> Value {
    if existing.iter().any(|old| {
        old.parameter.name == parameter.name
            && old.parameter.kind.trim_end_matches('?') == parameter.kind.trim_end_matches('?')
    }) {
        json!({"mode":"keep","value":parameter.name})
    } else {
        default_argument(parameter)
    }
}

pub(crate) async fn configure(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let file = source(&jobs.projects, project, input)?;
    let options = options(jobs, project, &file, input).await?;
    let call = &options.selection.call;
    let existing = existing(call, &options.previous)?;
    let parameters: Vec<_> = options
        .parameters
        .iter()
        .map(|parameter| parameter_form(parameter, initial_argument(parameter, &existing)))
        .collect();
    let old: Vec<_> = existing
        .iter()
        .map(|value| {
            let expression = call.arguments[value.index].expression;
            let preview: String = expression.chars().take(360).collect();
            json!({
                "name":value.parameter.name,
                "kind":value.parameter.kind,
                "expression":preview,
                "truncated":preview.len() < expression.len()
            })
        })
        .collect();
    Ok(json!({
        "current": display_name(call.class, call.constructor),
        "entry": entry(&options.selection, &options.constructor),
        "signature": options.signature["label"],
        "signatureRevision": options.revision()?,
        "parameters": parameters,
        "existing": old
    }))
}

fn selected_arguments(
    arguments: &Value,
    existing: &[Existing<'_>],
    parameters: &[Parameter],
) -> Result<Vec<Value>> {
    if !arguments.is_null()
        && arguments.as_object().is_none_or(|arguments| {
            arguments
                .keys()
                .any(|name| !parameters.iter().any(|parameter| parameter.name == *name))
        })
    {
        return Err(Problem::new(
            400,
            "Choose argument names from the constructor parameters.",
        ));
    }

    Ok(parameters
        .iter()
        .map(|parameter| {
            arguments
                .get(&parameter.name)
                .cloned()
                .unwrap_or_else(|| initial_argument(parameter, existing))
        })
        .collect())
}

fn destinations<'a>(
    arguments: &[Value],
    existing: &[Existing<'_>],
    parameters: &'a [Parameter],
    namespace: &str,
) -> Result<(Vec<Destination<'a>>, Vec<String>)> {
    let mut retained = HashSet::new();
    let mut destinations = vec![];
    let mut positional_gap = false;
    for (parameter, value) in parameters.iter().zip(arguments) {
        let value = if value["mode"] == "keep" {
            let old = existing
                .iter()
                .find(|old| value["value"] == old.parameter.name)
                .ok_or_else(|| Problem::new(422, "Choose a value from the current constructor."))?;
            if !retained.insert(old.index) {
                return Err(Problem::new(422, "Use each existing value at most once."));
            }
            Some(ValueSource::Existing(old.index))
        } else {
            argument(parameter, value, namespace)?.map(ValueSource::Expression)
        };
        if let Some(value) = value {
            if !parameter.named && positional_gap {
                return Err(Problem::new(
                    422,
                    "Supply the earlier positional arguments first.",
                ));
            }
            destinations.push(Destination {
                name: parameter.named.then_some(parameter.name.as_str()),
                value,
            });
        } else if !parameter.named {
            positional_gap = true;
        }
    }
    // Keep the original evaluation order where possible. Positional
    // arguments fill the available positions in new signature order.
    let source_order = |arg: &Destination<'_>| match arg.value {
        ValueSource::Existing(old) => old,
        ValueSource::Expression(_) => usize::MAX,
    };
    let mut positional: Vec<_> = destinations
        .iter()
        .filter(|arg| arg.name.is_none())
        .map(source_order)
        .collect();
    positional.sort_unstable();
    let mut positional = positional.into_iter();
    let mut ordered: Vec<_> = destinations
        .into_iter()
        .map(|arg| {
            let order = if arg.name.is_none() {
                positional.next().unwrap()
            } else {
                source_order(&arg)
            };
            (order, arg)
        })
        .collect();
    ordered.sort_by_key(|(order, _)| *order);
    let removed = existing
        .iter()
        .filter(|old| !retained.contains(&old.index))
        .map(|old| old.parameter.name.clone())
        .collect();
    Ok((ordered.into_iter().map(|(_, arg)| arg).collect(), removed))
}

pub(crate) async fn apply(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    save: bool,
) -> Result<Value> {
    let file = source(&jobs.projects, project, input)?;
    let options = options(jobs, project, &file, input).await?;
    if input["signatureRevision"] != options.revision()? {
        return Err(Problem::new(
            409,
            "The constructor parameters changed. Choose the constructor again.",
        ));
    }
    let call = &options.selection.call;
    let existing = existing(call, &options.previous)?;
    let choices = selected_arguments(&input["arguments"], &existing, &options.parameters)?;
    let import = needs_framework(choices.iter().zip(&options.parameters))
        .then(|| framework_namespace(&file.content))
        .transpose()?;
    let namespace = import
        .as_ref()
        .map_or("", |(_, namespace, _)| namespace.as_str());
    let (arguments, removed) = destinations(&choices, &existing, &options.parameters, namespace)?;
    if save && !removed.is_empty() && input["allowDrop"] != true {
        return Err(Problem::new(
            422,
            format!(
                "Confirm the removal of {} before saving.",
                removed.join(", ")
            ),
        ));
    }
    let (content, selected) = match import {
        Some((text, _, shift)) if shift > 0 => {
            let id = (call.id.parse::<usize>().unwrap() + shift).to_string();
            Call::locate(&text, &id, &options.selection.scope)?
                .rewrite(&options.constructor, &arguments)?
        }
        _ => call.rewrite(&options.constructor, &arguments)?,
    };
    let definition = options.selection.definition;
    let definition_revision = options.selection.revision;
    finish(
        jobs,
        project,
        input,
        Prepared {
            file,
            definition,
            definition_revision,
            content,
            selected,
        },
        save,
    )
    .await
}
