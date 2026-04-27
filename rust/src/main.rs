use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use chrono::DateTime;
use clap::{Args, Parser, Subcommand};
use globset::{Glob, GlobSet, GlobSetBuilder};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use plotters::prelude::*;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "git-of-theseus-rs")]
#[command(about = "Rust rewrite of git-of-theseus analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Analyze(AnalyzeArgs),
    #[command(name = "stack-plot")]
    StackPlot(StackPlotArgs),
    #[command(name = "line-plot")]
    LinePlot(LinePlotArgs),
    #[command(name = "survival-plot")]
    SurvivalPlot(SurvivalPlotArgs),
}

#[derive(Args, Clone)]
struct AnalyzeArgs {
    #[arg(default_value = ".")]
    repo_dir: PathBuf,

    #[arg(long, default_value = "%Y")]
    cohortfm: String,

    #[arg(long, default_value_t = 7 * 24 * 60 * 60)]
    interval: i64,

    #[arg(long, value_delimiter = ',')]
    ignore: Vec<String>,

    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,

    #[arg(long, default_value = ".")]
    outdir: PathBuf,

    #[arg(long, default_value = "master")]
    branch: String,

    #[arg(long, default_value_t = false)]
    all_filetypes: bool,

    #[arg(long, default_value_t = false)]
    ignore_whitespace: bool,

    #[arg(long)]
    procs: Option<usize>,

    #[arg(long, default_value_t = false)]
    quiet: bool,
}

#[derive(Args, Clone)]
struct StackPlotArgs {
    input_fn: PathBuf,

    #[arg(long, default_value_t = false)]
    display: bool,

    #[arg(long, default_value = "stack_plot.png")]
    outfile: PathBuf,

    #[arg(long, default_value_t = 20)]
    max_n: usize,

    #[arg(long, default_value_t = false)]
    normalize: bool,
}

#[derive(Args, Clone)]
struct LinePlotArgs {
    input_fn: PathBuf,

    #[arg(long, default_value_t = false)]
    display: bool,

    #[arg(long, default_value = "line_plot.png")]
    outfile: PathBuf,

    #[arg(long, default_value_t = 20)]
    max_n: usize,

    #[arg(long, default_value_t = false)]
    normalize: bool,
}

#[derive(Args, Clone)]
struct SurvivalPlotArgs {
    input_fns: Vec<PathBuf>,

    #[arg(long, default_value_t = false)]
    exp_fit: bool,

    #[arg(long, default_value_t = false)]
    display: bool,

    #[arg(long, default_value = "survival_plot.png")]
    outfile: PathBuf,

    #[arg(long, default_value_t = 5.0)]
    years: f64,
}

#[derive(Debug, Clone)]
struct CommitMeta {
    ts: i64,
    author: String,
    email: String,
}

#[derive(Debug, Clone)]
struct CommitRef {
    sha: String,
    ts: i64,
}

#[derive(Debug, Clone)]
struct TreeEntry {
    path: String,
    blob_sha: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct Key {
    category: String,
    label: String,
}

#[derive(Serialize)]
struct CurvesOut {
    y: Vec<Vec<i64>>,
    ts: Vec<String>,
    labels: Vec<String>,
}

#[derive(Deserialize)]
struct CurvesIn {
    y: Vec<Vec<f64>>,
    ts: Vec<String>,
    labels: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Analyze(args) => analyze(args),
        Commands::StackPlot(args) => stack_plot(args),
        Commands::LinePlot(args) => line_plot(args),
        Commands::SurvivalPlot(args) => survival_plot(args),
    }
}

fn analyze(args: AnalyzeArgs) -> Result<()> {
    let procs = args
        .procs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1)));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(procs)
        .build()
        .context("failed to build rayon pool")?;

    fs::create_dir_all(&args.outdir).with_context(|| format!("create {}", args.outdir.display()))?;

    let commit_meta = load_commit_meta(&args.repo_dir, &args.branch)?;
    if commit_meta.is_empty() {
        bail!("no commits found on branch {}", args.branch);
    }

    let mut first_parent = load_first_parent_commits(&args.repo_dir, &args.branch)?;
    if first_parent.is_empty() {
        bail!("no first-parent commits found on branch {}", args.branch);
    }
    first_parent.reverse(); // oldest -> newest

    let sampled = sample_by_interval(&first_parent, args.interval, &commit_meta);
    let mut sampled_refs: Vec<CommitRef> = sampled
        .into_iter()
        .filter_map(|sha| {
            commit_meta.get(&sha).map(|m| CommitRef {
                sha,
                ts: m.ts,
            })
        })
        .collect();
    sampled_refs.sort_by_key(|c| c.ts);

    let mut commit2cohort: HashMap<String, String> = HashMap::new();
    let mut curve_key_set: BTreeSet<Key> = BTreeSet::new();

    for (sha, meta) in &commit_meta {
        let cohort = cohort_from_ts(meta.ts, &args.cohortfm)?;
        commit2cohort.insert(sha.clone(), cohort.clone());
        curve_key_set.insert(Key {
            category: "cohort".to_string(),
            label: cohort,
        });
        curve_key_set.insert(Key {
            category: "author".to_string(),
            label: meta.author.clone(),
        });
        curve_key_set.insert(Key {
            category: "domain".to_string(),
            label: email_domain(&meta.email),
        });
    }

    let matcher = build_matcher(&args.only, &args.ignore)?;
    let mut last_file_hash: HashMap<String, String> = HashMap::new();
    let mut last_file_y: HashMap<String, HashMap<Key, i64>> = HashMap::new();
    let mut cur_y: HashMap<Key, i64> = HashMap::new();
    let mut commit_history: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    let mut ts: Vec<String> = Vec::new();
    let mut curves: HashMap<Key, Vec<i64>> = HashMap::new();
    let progress = if args.quiet {
        None
    } else {
        let mp = MultiProgress::new();
        let commit_pb = mp.add(ProgressBar::new(sampled_refs.len() as u64));
        let commit_style = ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}",
        )
        .context("invalid progress style template")?
        .progress_chars("##-");
        commit_pb.set_style(commit_style);
        commit_pb.set_message(format!("commits (workers={procs})"));

        let file_pb = mp.add(ProgressBar::new(0));
        let file_style = ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.green/black} {pos}/{len} {msg}",
        )
        .context("invalid progress style template")?
        .progress_chars("##-");
        file_pb.set_style(file_style);
        file_pb.set_message("files blamed");

        Some((mp, commit_pb, file_pb))
    };

    for c in &sampled_refs {
        let t = DateTime::from_timestamp(c.ts, 0)
            .ok_or_else(|| anyhow!("bad timestamp {}", c.ts))?
            .to_rfc3339();
        ts.push(t);

        let entries = list_tree_entries(&args.repo_dir, &c.sha, args.all_filetypes, &matcher)?;
        let mut cur_hash: HashMap<String, String> = HashMap::new();
        let mut check_paths: Vec<TreeEntry> = Vec::new();

        for entry in entries {
            curve_key_set.insert(Key {
                category: "ext".to_string(),
                label: file_ext(&entry.path),
            });
            curve_key_set.insert(Key {
                category: "dir".to_string(),
                label: top_dir(&entry.path),
            });

            cur_hash.insert(entry.path.clone(), entry.blob_sha.clone());
            if let Some(prev_sha) = last_file_hash.get(&entry.path) {
                if prev_sha != &entry.blob_sha {
                    if let Some(prev_hist) = last_file_y.get(&entry.path) {
                        for (k, v) in prev_hist {
                            *cur_y.entry(k.clone()).or_insert(0) -= *v;
                        }
                    }
                    check_paths.push(entry);
                }
            } else {
                check_paths.push(entry);
            }
        }

        for deleted_path in last_file_hash.keys() {
            if !cur_hash.contains_key(deleted_path) {
                if let Some(prev_hist) = last_file_y.get(deleted_path) {
                    for (k, v) in prev_hist {
                        *cur_y.entry(k.clone()).or_insert(0) -= *v;
                    }
                }
            }
        }

        if let Some((_, _, file_pb)) = &progress {
            file_pb.inc_length(check_paths.len() as u64);
            file_pb.set_message(format!(
                "files blamed in {}",
                c.sha.chars().take(8).collect::<String>()
            ));
        }

        let file_pb_for_threads = progress.as_ref().map(|(_, _, pb)| pb.clone());
        let blame_results: Vec<(String, HashMap<Key, i64>)> = pool.install(|| {
            check_paths
                .par_iter()
                .filter_map(|entry| {
                    let hist = file_histogram(
                        &args.repo_dir,
                        &c.sha,
                        &entry.path,
                        args.ignore_whitespace,
                        &commit2cohort,
                        &commit_meta,
                    )
                    .ok()?;
                    if let Some(pb) = &file_pb_for_threads {
                        pb.inc(1);
                    }
                    Some((entry.path.clone(), hist))
                })
                .collect()
        });

        for (path, hist) in blame_results {
            for (k, v) in &hist {
                *cur_y.entry(k.clone()).or_insert(0) += *v;
            }
            last_file_y.insert(path, hist);
        }

        last_file_hash = cur_hash;

        for (k, v) in &cur_y {
            if k.category == "sha" {
                commit_history
                    .entry(k.label.clone())
                    .or_default()
                    .push((c.ts, *v));
            }
        }
        for k in &curve_key_set {
            curves.entry(k.clone()).or_default().push(*cur_y.get(k).unwrap_or(&0));
        }

        if let Some((_, commit_pb, file_pb)) = &progress {
            commit_pb.inc(1);
            commit_pb.set_message(format!(
                "last commit {} changed file(s), workers={}",
                check_paths.len(),
                procs
            ));
            file_pb.set_message(format!(
                "last commit {} blamed file(s)",
                check_paths.len()
            ));
        }
    }
    if let Some((_, commit_pb, file_pb)) = &progress {
        commit_pb.finish_with_message("commit analysis complete");
        file_pb.finish_with_message("file blame analysis complete");
    }

    dump_curves(&args.outdir, "cohorts.json", "cohort", &curve_key_set, &curves, &ts)?;
    dump_curves(&args.outdir, "exts.json", "ext", &curve_key_set, &curves, &ts)?;
    dump_curves(&args.outdir, "authors.json", "author", &curve_key_set, &curves, &ts)?;
    dump_curves(&args.outdir, "dirs.json", "dir", &curve_key_set, &curves, &ts)?;
    dump_curves(&args.outdir, "domains.json", "domain", &curve_key_set, &curves, &ts)?;

    let survival_path = args.outdir.join("survival.json");
    let f = File::create(&survival_path)
        .with_context(|| format!("create {}", survival_path.display()))?;
    serde_json::to_writer(f, &commit_history)
        .with_context(|| format!("write {}", survival_path.display()))?;
    Ok(())
}

fn load_commit_meta(repo: &PathBuf, branch: &str) -> Result<HashMap<String, CommitMeta>> {
    let out = git_output(
        repo,
        &[
            "log",
            "--format=%H%x09%ct%x09%an%x09%ae",
            branch,
        ],
    )?;
    let mut map = HashMap::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() != 4 {
            continue;
        }
        let ts = parts[1].parse::<i64>().unwrap_or(0);
        map.insert(
            parts[0].to_string(),
            CommitMeta {
                ts,
                author: parts[2].to_string(),
                email: parts[3].to_string(),
            },
        );
    }
    Ok(map)
}

fn load_first_parent_commits(repo: &PathBuf, branch: &str) -> Result<Vec<String>> {
    let out = git_output(repo, &["rev-list", "--first-parent", branch])?;
    Ok(out.lines().map(|x| x.to_string()).collect())
}

fn sample_by_interval(
    commits_old_to_new: &[String],
    interval: i64,
    commit_meta: &HashMap<String, CommitMeta>,
) -> Vec<String> {
    if interval <= 0 {
        return commits_old_to_new.to_vec();
    }
    let mut out = Vec::new();
    let mut last_ts: Option<i64> = None;
    for sha in commits_old_to_new {
        let ts = if let Some(meta) = commit_meta.get(sha) {
            meta.ts
        } else {
            continue;
        };
        if last_ts.is_none() || ts >= last_ts.unwrap_or(0) + interval {
            out.push(sha.clone());
            last_ts = Some(ts);
        }
    }
    if out.is_empty() {
        commits_old_to_new
            .first()
            .map_or_else(Vec::new, |c| vec![c.clone()])
    } else {
        out
    }
}

fn build_matcher(only: &[String], ignore: &[String]) -> Result<Option<(GlobSet, GlobSet)>> {
    if only.is_empty() && ignore.is_empty() {
        return Ok(None);
    }
    let mut only_builder = GlobSetBuilder::new();
    for p in only {
        only_builder.add(Glob::new(p).with_context(|| format!("invalid only glob {}", p))?);
    }
    let mut ignore_builder = GlobSetBuilder::new();
    for p in ignore {
        ignore_builder.add(Glob::new(p).with_context(|| format!("invalid ignore glob {}", p))?);
    }
    Ok(Some((only_builder.build()?, ignore_builder.build()?)))
}

fn list_tree_entries(
    repo: &PathBuf,
    commit: &str,
    all_filetypes: bool,
    matcher: &Option<(GlobSet, GlobSet)>,
) -> Result<Vec<TreeEntry>> {
    let out = git_output(repo, &["ls-tree", "-r", "--full-tree", commit])?;
    let mut v = Vec::new();
    for line in out.lines() {
        let mut split = line.splitn(2, '\t');
        let left = split.next().unwrap_or_default();
        let path = split.next().unwrap_or_default().to_string();
        let mut fields = left.split_whitespace();
        let _mode = fields.next();
        let entry_type = fields.next().unwrap_or_default();
        let blob_sha = fields.next().unwrap_or_default().to_string();
        if entry_type != "blob" {
            continue;
        }
        if !all_filetypes && !is_probably_code(&path) {
            continue;
        }
        if !path_ok(&path, matcher) {
            continue;
        }
        v.push(TreeEntry { path, blob_sha });
    }
    Ok(v)
}

fn path_ok(path: &str, matcher: &Option<(GlobSet, GlobSet)>) -> bool {
    match matcher {
        None => true,
        Some((only, ignore)) => {
            let only_ok = only.len() == 0 || only.is_match(path);
            let ignore_ok = !ignore.is_match(path);
            only_ok && ignore_ok
        }
    }
}

fn is_probably_code(path: &str) -> bool {
    let ext = file_ext(path);
    let s = ext.as_str();
    let allowed: HashSet<&str> = HashSet::from([
        "", "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "rs", "go", "py", "java", "kt", "scala",
        "js", "jsx", "ts", "tsx", "rb", "php", "swift", "m", "mm", "cs", "sql", "sh", "bash",
        "zsh", "fish", "lua", "pl", "pm", "r", "dart", "vue", "svelte", "toml", "ini", "cfg",
        "conf", "yaml", "yml",
    ]);
    allowed.contains(s)
}

fn file_histogram(
    repo: &PathBuf,
    commit: &str,
    path: &str,
    ignore_whitespace: bool,
    commit2cohort: &HashMap<String, String>,
    commit_meta: &HashMap<String, CommitMeta>,
) -> Result<HashMap<Key, i64>> {
    let mut args = vec!["blame", "--line-porcelain"];
    if ignore_whitespace {
        args.push("-w");
    }
    args.push(commit);
    args.push("--");
    args.push(path);
    let out = git_output(repo, &args)?;
    let mut counts: HashMap<String, i64> = HashMap::new();
    for line in out.lines() {
        // Hunk header starts with "<sha> <orig> <final> <num>"
        let first = line.split_whitespace().next().unwrap_or_default();
        if first.len() == 40 && first.chars().all(|c| c.is_ascii_hexdigit()) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let n = parts[3].parse::<i64>().unwrap_or(0);
                *counts.entry(parts[0].to_string()).or_insert(0) += n;
            }
        }
    }

    let mut h: HashMap<Key, i64> = HashMap::new();
    let ext = file_ext(path);
    let dir = top_dir(path);
    for (blame_sha, n) in counts {
        let cohort = commit2cohort
            .get(&blame_sha)
            .cloned()
            .unwrap_or_else(|| "MISSING".to_string());
        let meta = commit_meta.get(&blame_sha).cloned().unwrap_or(CommitMeta {
            ts: 0,
            author: "UNKNOWN".to_string(),
            email: "unknown@unknown".to_string(),
        });
        let domain = email_domain(&meta.email);
        add_count(&mut h, "cohort", &cohort, n);
        add_count(&mut h, "ext", &ext, n);
        add_count(&mut h, "author", &meta.author, n);
        add_count(&mut h, "dir", &dir, n);
        add_count(&mut h, "domain", &domain, n);
        if commit2cohort.contains_key(&blame_sha) {
            add_count(&mut h, "sha", &blame_sha, n);
        }
    }
    Ok(h)
}

fn cohort_from_ts(ts: i64, fmt: &str) -> Result<String> {
    let dt = DateTime::from_timestamp(ts, 0).ok_or_else(|| anyhow!("bad timestamp {}", ts))?;
    Ok(dt.format(fmt).to_string())
}

fn add_count(map: &mut HashMap<Key, i64>, category: &str, label: &str, n: i64) {
    *map.entry(Key {
        category: category.to_string(),
        label: label.to_string(),
    })
    .or_insert(0) += n;
}

fn file_ext(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.extension()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn top_dir(path: &str) -> String {
    path.split('/').next().unwrap_or_default().to_string() + "/"
}

fn email_domain(email: &str) -> String {
    email
        .split('@')
        .nth(1)
        .unwrap_or("unknown")
        .to_string()
}

fn dump_curves(
    outdir: &PathBuf,
    filename: &str,
    category: &str,
    key_set: &BTreeSet<Key>,
    curves: &HashMap<Key, Vec<i64>>,
    ts: &[String],
) -> Result<()> {
    let mut labels: Vec<String> = key_set
        .iter()
        .filter(|k| k.category == category)
        .map(|k| k.label.clone())
        .collect();
    labels.sort();
    let y: Vec<Vec<i64>> = labels
        .iter()
        .map(|label| {
            curves
                .get(&Key {
                    category: category.to_string(),
                    label: label.clone(),
                })
                .cloned()
                .unwrap_or_else(|| vec![0; ts.len()])
        })
        .collect();
    let out = CurvesOut {
        y,
        ts: ts.to_vec(),
        labels,
    };
    let path = outdir.join(filename);
    let f = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer(f, &out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn stack_plot(args: StackPlotArgs) -> Result<()> {
    let _ = args.display; // Kept for CLI compatibility; plotting is file-based.
    let data = read_curves(&args.input_fn)?;
    if data.y.is_empty() || data.ts.is_empty() {
        bail!("input dataset is empty");
    }
    let (mut y, labels) = top_n_with_other(data.y, data.labels, args.max_n);
    if args.normalize {
        normalize_columns(&mut y);
    }

    let n = data.ts.len() as i32;
    let mut cumulative = vec![0.0_f64; data.ts.len()];
    let mut y_max = 0.0_f64;
    for series in &y {
        for (i, v) in series.iter().enumerate() {
            cumulative[i] += *v;
            y_max = y_max.max(cumulative[i]);
        }
    }
    if y_max <= 0.0 {
        y_max = if args.normalize { 100.0 } else { 1.0 };
    }

    let root = BitMapBackend::new(&args.outfile, (1920, 1280)).into_drawing_area();
    root.fill(&WHITE)?;
    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .caption("Git of Theseus Stack Plot", ("sans-serif", 36))
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0i32..(n.max(2) - 1), 0f64..(y_max * 1.05))?;
    chart.configure_mesh().draw()?;

    let colors = generate_n_colors(labels.len());
    let mut lower = vec![0.0_f64; data.ts.len()];
    for (series_idx, series) in y.iter().enumerate() {
        let mut upper = lower.clone();
        for (i, v) in series.iter().enumerate() {
            upper[i] += *v;
        }
        let mut poly: Vec<(i32, f64)> = upper
            .iter()
            .enumerate()
            .map(|(i, v)| (i as i32, *v))
            .collect();
        let mut lower_rev: Vec<(i32, f64)> = lower
            .iter()
            .enumerate()
            .rev()
            .map(|(i, v)| (i as i32, *v))
            .collect();
        poly.append(&mut lower_rev);
        chart
            .draw_series(std::iter::once(Polygon::new(
                poly,
                colors[series_idx].mix(0.35).filled(),
            )))?
            .label(labels[series_idx].clone())
            .legend({
                let c = colors[series_idx];
                move |(x, y)| Rectangle::new([(x, y - 5), (x + 12, y + 5)], c.filled())
            });
        lower = upper;
    }

    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;
    root.present()?;
    Ok(())
}

fn line_plot(args: LinePlotArgs) -> Result<()> {
    let _ = args.display; // Kept for CLI compatibility; plotting is file-based.
    let data = read_curves(&args.input_fn)?;
    if data.y.is_empty() || data.ts.is_empty() {
        bail!("input dataset is empty");
    }
    let (mut y, labels) = top_n(data.y, data.labels, args.max_n);
    if args.normalize {
        normalize_columns(&mut y);
    }

    let n = data.ts.len() as i32;
    let mut y_max = y
        .iter()
        .flat_map(|s| s.iter().copied())
        .fold(0.0_f64, f64::max);
    if y_max <= 0.0 {
        y_max = if args.normalize { 100.0 } else { 1.0 };
    }

    let root = BitMapBackend::new(&args.outfile, (1920, 1280)).into_drawing_area();
    root.fill(&WHITE)?;
    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .caption("Git of Theseus Line Plot", ("sans-serif", 36))
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0i32..(n.max(2) - 1), 0f64..(y_max * 1.05))?;
    chart.configure_mesh().draw()?;

    let colors = generate_n_colors(labels.len());
    for (idx, series) in y.iter().enumerate() {
        let c = colors[idx];
        chart
            .draw_series(LineSeries::new(
                series.iter().enumerate().map(|(i, v)| (i as i32, *v)),
                &c,
            ))?
            .label(labels[idx].clone())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 12, y)], c));
    }
    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;
    root.present()?;
    Ok(())
}

fn survival_plot(args: SurvivalPlotArgs) -> Result<()> {
    let _ = args.display; // Kept for CLI compatibility; plotting is file-based.
    if args.input_fns.is_empty() {
        bail!("please provide at least one survival.json file");
    }
    let year_seconds = 365.25 * 24.0 * 60.0 * 60.0;
    let root = BitMapBackend::new(&args.outfile, (1920, 1280)).into_drawing_area();
    root.fill(&WHITE)?;
    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .caption("Code Survival Plot", ("sans-serif", 36))
        .x_label_area_size(50)
        .y_label_area_size(60)
        .build_cartesian_2d(0f64..args.years, 0f64..100f64)?;
    chart.configure_mesh().x_desc("Years").y_desc("%").draw()?;

    let mut all_deltas: Vec<(f64, HashMap<i64, (f64, f64)>)> = Vec::new();
    let colors = generate_n_colors(args.input_fns.len());

    for (idx, fn_path) in args.input_fns.iter().enumerate() {
        let commit_history: HashMap<String, Vec<(i64, f64)>> = read_json(fn_path)?;
        let mut deltas: HashMap<i64, (f64, f64)> = HashMap::new();
        let mut total_n = 0.0_f64;
        for history in commit_history.values() {
            if history.is_empty() {
                continue;
            }
            let (t0, orig_count) = history[0];
            total_n += orig_count;
            let mut last_count = orig_count;
            for (t, count) in history.iter().skip(1) {
                let e = deltas.entry(*t - t0).or_insert((0.0, 0.0));
                e.0 += *count - last_count;
                last_count = *count;
            }
            if let Some((t_last, _)) = history.last().copied() {
                let e = deltas.entry(t_last - t0).or_insert((0.0, 0.0));
                e.0 += -last_count;
                e.1 += -orig_count;
            }
        }
        all_deltas.push((total_n, deltas.clone()));
        let (xs, ys) = compute_km_curve(total_n, &deltas, year_seconds);
        let c = colors[idx];
        let label = fn_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_else(|| fn_path.to_string_lossy().to_string());
        chart
            .draw_series(LineSeries::new(xs.into_iter().zip(ys.into_iter()), &c))?
            .label(label)
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 12, y)], c));
    }

    if args.exp_fit {
        let k = fit_exponential(&all_deltas, year_seconds);
        let points: Vec<(f64, f64)> = (0..1000)
            .map(|i| {
                let x = args.years * (i as f64) / 999.0;
                (x, 100.0 * f64::exp(-k * x))
            })
            .collect();
        chart
            .draw_series(LineSeries::new(points, &RED))?
            .label(format!("Exponential fit (half-life {:.2}y)", f64::ln(2.0) / k))
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 12, y)], RED));
    }

    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;
    root.present()?;
    Ok(())
}

fn compute_km_curve(
    mut total_n: f64,
    deltas: &HashMap<i64, (f64, f64)>,
    year_seconds: f64,
) -> (Vec<f64>, Vec<f64>) {
    let mut keys: Vec<i64> = deltas.keys().copied().collect();
    keys.sort_unstable();
    let mut p = 1.0_f64;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for t in keys {
        let (delta_k, delta_n) = deltas.get(&t).copied().unwrap_or((0.0, 0.0));
        xs.push((t as f64) / year_seconds);
        ys.push(100.0 * p);
        if total_n != 0.0 {
            p *= 1.0 + delta_k / total_n;
        }
        total_n += delta_n;
        if p < 0.05 {
            break;
        }
    }
    (xs, ys)
}

fn fit_exponential(all_deltas: &[(f64, HashMap<i64, (f64, f64)>)], year_seconds: f64) -> f64 {
    let mut best_k = 0.5_f64;
    let mut best_loss = f64::INFINITY;
    for i in 1..=4000 {
        let k = (i as f64) * 0.0025;
        let loss = exponential_loss(k, all_deltas, year_seconds);
        if loss < best_loss {
            best_loss = loss;
            best_k = k;
        }
    }
    best_k
}

fn exponential_loss(k: f64, all_deltas: &[(f64, HashMap<i64, (f64, f64)>)], year_seconds: f64) -> f64 {
    let mut loss = 0.0_f64;
    for (start_n, deltas) in all_deltas {
        let mut total_n = *start_n;
        let mut p = 1.0_f64;
        let mut keys: Vec<i64> = deltas.keys().copied().collect();
        keys.sort_unstable();
        for t in keys {
            let (delta_k, delta_n) = deltas.get(&t).copied().unwrap_or((0.0, 0.0));
            let years = (t as f64) / year_seconds;
            let pred = start_n * f64::exp(-k * years);
            loss += (start_n * p - pred).powi(2);
            if total_n != 0.0 {
                p *= 1.0 + delta_k / total_n;
            }
            total_n += delta_n;
        }
    }
    loss
}

fn top_n(mut y: Vec<Vec<f64>>, labels: Vec<String>, max_n: usize) -> (Vec<Vec<f64>>, Vec<String>) {
    if y.len() <= max_n {
        return (y, labels);
    }
    let mut idxs: Vec<usize> = (0..y.len()).collect();
    idxs.sort_by(|a, b| {
        y[*b]
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .partial_cmp(&y[*a].iter().copied().fold(0.0_f64, f64::max))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top = idxs.into_iter().take(max_n).collect::<Vec<_>>();
    top.sort_by_key(|j| labels[*j].clone());
    let y_out = top.iter().map(|j| y[*j].clone()).collect::<Vec<_>>();
    let labels_out = top.iter().map(|j| labels[*j].clone()).collect::<Vec<_>>();
    y.clear();
    (y_out, labels_out)
}

fn top_n_with_other(
    y: Vec<Vec<f64>>,
    labels: Vec<String>,
    max_n: usize,
) -> (Vec<Vec<f64>>, Vec<String>) {
    if y.len() <= max_n {
        return (y, labels);
    }
    let mut idxs: Vec<usize> = (0..y.len()).collect();
    idxs.sort_by(|a, b| {
        y[*b]
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .partial_cmp(&y[*a].iter().copied().fold(0.0_f64, f64::max))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top = idxs.iter().copied().take(max_n).collect::<Vec<_>>();
    let rest = idxs.iter().copied().skip(max_n).collect::<Vec<_>>();
    let mut top_sorted = top;
    top_sorted.sort_by_key(|j| labels[*j].clone());
    let mut y_out = top_sorted.iter().map(|j| y[*j].clone()).collect::<Vec<_>>();
    let mut labels_out = top_sorted
        .iter()
        .map(|j| labels[*j].clone())
        .collect::<Vec<_>>();
    if let Some(width) = y.first().map(|s| s.len()) {
        let mut other = vec![0.0_f64; width];
        for j in rest {
            for (i, v) in y[j].iter().enumerate() {
                other[i] += *v;
            }
        }
        y_out.push(other);
        labels_out.push("other".to_string());
    }
    (y_out, labels_out)
}

fn normalize_columns(y: &mut [Vec<f64>]) {
    if y.is_empty() {
        return;
    }
    let cols = y[0].len();
    for c in 0..cols {
        let sum: f64 = y.iter().map(|row| row[c]).sum();
        if sum > 0.0 {
            for row in y.iter_mut() {
                row[c] = 100.0 * row[c] / sum;
            }
        }
    }
}

fn generate_n_colors(n: usize) -> Vec<RGBColor> {
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| {
            let hue = (i as f64) / (n as f64);
            hsl_to_rgb(hue, 0.65, 0.50)
        })
        .collect()
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> RGBColor {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    RGBColor((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

fn read_curves(path: &PathBuf) -> Result<CurvesIn> {
    read_json(path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Result<T> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(f).with_context(|| format!("parse {}", path.display()))
}

fn git_output(repo: &PathBuf, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn git {:?}", args))?;
    let stdout = child.stdout.take().context("capture stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut out = String::new();
    reader
        .read_to_string(&mut out)
        .with_context(|| format!("read git output {:?}", args))?;
    let status = child.wait().with_context(|| format!("wait git {:?}", args))?;
    if !status.success() {
        bail!("git command failed: git {:?}", args);
    }
    Ok(out)
}

