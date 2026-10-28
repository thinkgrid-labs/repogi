//! Read-only Git plumbing for change impact. Every call is an argument vector
//! (no shell), with hooks, fsmonitor, external diff, textconv and replace
//! objects disabled. Blob content is read raw (`cat-file --batch`,
//! `hash-object --no-filters`), so no clean/smudge filter or other configured
//! command runs. Nothing here writes to the repository, its index or its refs.
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use anyhow::{Context, Result, bail, ensure};

/// A Git work tree and the Atlas root inside it.
#[derive(Debug, Clone)]
pub struct Git {
    /// Canonical Atlas root (the directory analysed), at or below the top level.
    pub root: PathBuf,
    /// Canonical work-tree top level.
    pub toplevel: PathBuf,
    /// The root relative to the top level, with a trailing `/`, or empty.
    pub prefix: String,
    /// Repository discovery may not look above this directory
    /// (`GIT_CEILING_DIRECTORIES`); set by [`Git::open_confined`].
    ceiling: Option<PathBuf>,
}

/// A resolved commit and the tree of the Atlas root inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub spec: String,
    pub commit: String,
    /// Tree object of the Atlas root at this commit.
    pub tree: String,
}

/// One blob entry of a tree, path relative to the Atlas root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub oid: String,
}

/// A rename recorded by Git's similarity detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
    pub similarity: u32,
}

impl Git {
    pub fn open(root: &Path) -> Result<Self> {
        Self::discover(root, false)
    }

    /// Like [`Git::open`], but `root` must itself be the work-tree top level:
    /// discovery never looks above it, so no repository outside the root is
    /// used (the MCP server's configured-root boundary).
    pub fn open_confined(root: &Path) -> Result<Self> {
        let git = Self::discover(root, true)?;
        ensure!(
            git.prefix.is_empty() && git.toplevel == git.root,
            "{} is not a Git work-tree top level; configure the repository root itself",
            git.root.display()
        );
        Ok(git)
    }

    fn discover(root: &Path, confined: bool) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("open repository {}", root.display()))?;
        let ceiling = confined
            .then(|| root.parent().map(Path::to_path_buf))
            .flatten();
        let output = run(
            &root,
            ceiling.as_deref(),
            &["rev-parse", "--show-toplevel", "--show-prefix"],
            None,
        )
        .context(if confined {
            "not a Git work tree at this root; the configured root must be the work-tree top level (repositories above it are not used)"
        } else {
            "not a Git work tree; `impact` compares Git revisions"
        })?;
        let text = String::from_utf8(output).context("non-UTF-8 Git path")?;
        let mut lines = text.lines();
        let toplevel = PathBuf::from(lines.next().context("git rev-parse: no top level")?)
            .canonicalize()
            .context("resolve Git top level")?;
        let prefix = lines.next().unwrap_or_default().to_owned();
        ensure!(
            prefix.is_empty() || prefix.ends_with('/'),
            "unexpected Git prefix {prefix:?}"
        );
        Ok(Self {
            root,
            toplevel,
            prefix,
            ceiling,
        })
    }

    fn git(&self, args: &[&str], input: Option<Vec<u8>>) -> Result<Vec<u8>> {
        run(&self.root, self.ceiling.as_deref(), args, input)
    }

    /// Resolve `spec` to a commit and the tree of the Atlas root in it.
    pub fn revision(&self, spec: &str) -> Result<Revision> {
        ensure!(
            !spec.is_empty() && !spec.contains('\0'),
            "empty or invalid revision"
        );
        let commit = self
            .git(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("{spec}^{{commit}}"),
                ],
                None,
            )
            .with_context(|| format!("unknown revision {spec:?}; pass a commit, branch or tag"))?;
        let commit = String::from_utf8(commit)?.trim().to_owned();
        ensure!(is_oid(&commit), "unexpected commit id {commit:?}");
        let tree_spec = if self.prefix.is_empty() {
            format!("{commit}^{{tree}}")
        } else {
            format!("{commit}:{}", self.prefix.trim_end_matches('/'))
        };
        let tree = self
            .git(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &tree_spec,
                ],
                None,
            )
            .with_context(|| format!("{spec} does not contain {}", self.prefix))?;
        let tree = String::from_utf8(tree)?.trim().to_owned();
        ensure!(is_oid(&tree), "unexpected tree id {tree:?}");
        Ok(Revision {
            spec: spec.to_owned(),
            commit,
            tree,
        })
    }

    /// Every entry of a tree, recursively; paths relative to that tree.
    /// `--full-tree`: without it, Git filters the listing by the current
    /// directory, which is the analysed directory, not a path inside `tree`.
    pub fn ls_tree(&self, tree: &str) -> Result<BTreeMap<String, Entry>> {
        ensure!(is_oid(tree), "invalid tree id");
        let output = self.git(
            &[
                "ls-tree",
                "-r",
                "-z",
                "--full-tree",
                "--end-of-options",
                tree,
            ],
            None,
        )?;
        let mut entries = BTreeMap::new();
        for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record).context("non-UTF-8 path in tree")?;
            let (meta, path) = record
                .split_once('\t')
                .context("malformed ls-tree output")?;
            let mut fields = meta.split(' ');
            let (Some(mode), Some(_kind), Some(oid)) =
                (fields.next(), fields.next(), fields.next())
            else {
                bail!("malformed ls-tree output");
            };
            entries.insert(
                path.to_owned(),
                Entry {
                    mode: mode.to_owned(),
                    oid: oid.to_owned(),
                },
            );
        }
        Ok(entries)
    }

    /// Raw blob contents, in request order (`cat-file --batch`, no filters).
    pub fn blobs(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        for oid in oids {
            ensure!(is_oid(oid), "invalid blob id");
        }
        let mut child = command(&self.root, self.ceiling.as_deref())
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("run git cat-file")?;
        let mut stdin = child.stdin.take().context("git stdin")?;
        let requests: Vec<u8> = oids
            .iter()
            .flat_map(|oid| format!("{oid}\n").into_bytes())
            .collect();
        let writer = thread::spawn(move || stdin.write_all(&requests));
        let mut reader = BufReader::new(child.stdout.take().context("git stdout")?);
        let mut blobs = Vec::with_capacity(oids.len());
        for oid in oids {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let fields: Vec<&str> = header.split_whitespace().collect();
            ensure!(
                fields.len() == 3 && fields[0] == oid && fields[1] == "blob",
                "git cat-file: unexpected object {oid}: {}",
                header.trim()
            );
            let size: usize = fields[2].parse()?;
            let mut content = vec![0; size];
            reader.read_exact(&mut content)?;
            let mut newline = [0; 1];
            reader.read_exact(&mut newline)?;
            blobs.push(content);
        }
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("git cat-file writer panicked"))??;
        let status = child.wait()?;
        ensure!(status.success(), "git cat-file failed");
        Ok(blobs)
    }

    /// Stage-0 Git index entries below the Atlas root (paths relative to it).
    /// Unmerged paths are reported with mode `unmerged`.
    pub fn index_entries(&self) -> Result<BTreeMap<String, Entry>> {
        let output = self.git(&["ls-files", "-s", "-z", "--full-name", "--", "."], None)?;
        let mut entries = BTreeMap::new();
        for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record).context("non-UTF-8 path in Git index")?;
            let (meta, path) = record
                .split_once('\t')
                .context("malformed ls-files output")?;
            let fields: Vec<&str> = meta.split(' ').collect();
            ensure!(fields.len() == 3, "malformed ls-files output");
            let Some(path) = path.strip_prefix(&self.prefix) else {
                continue;
            };
            let entry = Entry {
                mode: if fields[2] == "0" {
                    fields[0].to_owned()
                } else {
                    "unmerged".to_owned()
                },
                oid: fields[1].to_owned(),
            };
            entries.insert(path.to_owned(), entry);
        }
        Ok(entries)
    }

    /// Blob IDs of files on disk, without filters (`hash-object --no-filters`).
    pub fn hash_files(&self, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // Absolute paths: `--stdin-paths` resolves relative paths against the
        // work-tree top level, not against the analysed directory.
        let mut input = Vec::new();
        for path in paths {
            ensure!(
                !path.contains('\n'),
                "path with newline is unsupported: {path:?}"
            );
            let absolute = self.root.join(path);
            input.extend_from_slice(absolute.to_str().context("non-UTF-8 path")?.as_bytes());
            input.push(b'\n');
        }
        let output = self.git(
            &["hash-object", "--no-filters", "--stdin-paths"],
            Some(input),
        )?;
        let oids: Vec<String> = String::from_utf8(output)?
            .lines()
            .map(str::to_owned)
            .collect();
        ensure!(
            oids.len() == paths.len(),
            "git hash-object returned a wrong count"
        );
        Ok(oids)
    }

    /// `filter`, `text` and `eol` attributes of paths (relative to the Atlas
    /// root), from `.gitattributes` and `info/attributes`. Reads attributes only;
    /// no filter or other configured command runs.
    pub fn attributes(&self, paths: &[String]) -> Result<BTreeMap<String, Vec<(String, String)>>> {
        let mut attributes: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        if paths.is_empty() {
            return Ok(attributes);
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let output = self.git(
            &["check-attr", "-z", "--stdin", "filter", "text", "eol"],
            Some(input),
        )?;
        let fields: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
        for chunk in fields.chunks(3) {
            if let [path, attribute, value] = chunk {
                let value = String::from_utf8_lossy(value).into_owned();
                if value != "unspecified" && value != "unset" {
                    attributes
                        .entry(String::from_utf8_lossy(path).into_owned())
                        .or_default()
                        .push((String::from_utf8_lossy(attribute).into_owned(), value));
                }
            }
        }
        Ok(attributes)
    }

    /// A Git configuration value, when set (`git config --get`; reads only).
    #[must_use]
    pub fn config(&self, key: &str) -> Option<String> {
        self.git(&["config", "--get", "--end-of-options", key], None)
            .ok()
            .map(|value| String::from_utf8_lossy(&value).trim().to_owned())
    }

    /// Renames Git detects between two commits, or between `base` and the Git
    /// index (`head` = None: staged renames). Paths relative to the Atlas root.
    pub fn renames(&self, base: &Revision, head: Option<&Revision>) -> Result<Vec<Rename>> {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "-M",
            "--name-status",
            "-z",
            "--relative",
        ];
        if head.is_none() {
            args.push("--cached");
        }
        args.push(&base.commit);
        if let Some(head) = head {
            args.push(&head.commit);
        }
        args.extend(["--", "."]);
        let output = self.git(&args, None)?;
        let mut fields = output
            .split(|byte| *byte == 0)
            .filter(|r| !r.is_empty())
            .map(|field| std::str::from_utf8(field).context("non-UTF-8 path in diff"));
        let mut renames = Vec::new();
        while let Some(status) = fields.next() {
            let status = status?;
            if let Some(score) = status.strip_prefix('R') {
                let from = fields.next().context("malformed rename")??;
                let to = fields.next().context("malformed rename")??;
                renames.push(Rename {
                    from: from.to_owned(),
                    to: to.to_owned(),
                    similarity: score.parse().unwrap_or(0),
                });
            } else if status.starts_with('C') {
                fields.next();
                fields.next();
            } else {
                fields.next();
            }
        }
        Ok(renames)
    }
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn command(cwd: &Path, ceiling: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    if let Some(ceiling) = ceiling {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    command
        .current_dir(cwd)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "diff.external=",
            "-c",
            "protocol.allow=never",
            "--no-pager",
            "--no-optional-locks",
            "--no-replace-objects",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C");
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_EXTERNAL_DIFF",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(variable);
    }
    command
}

fn run(
    cwd: &Path,
    ceiling: Option<&Path>,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut child = command(cwd, ceiling)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run git; install Git to use `impact`")?;
    let writer = input.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || stdin.write_all(&input))
    });
    let output = child.wait_with_output()?;
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("git writer panicked"))??;
    }
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
