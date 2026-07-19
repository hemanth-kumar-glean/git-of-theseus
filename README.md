# Git of Theseus

Analyze Git repositories and generate plots that show how code changes over time. For example, running it on [Git](https://github.com/git/git) itself produces:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-git.png)

Installing
----------

Git of Theseus is installed from the Rust CLI in this repository:

```shell
cargo install --path rust --locked
```

This installs the following commands:

- `git-of-theseus-analyze`
- `git-of-theseus-stack-plot`
- `git-of-theseus-line-plot`
- `git-of-theseus-survival-plot`

You can also run directly from the repository without installing:

```shell
cargo run --manifest-path rust/Cargo.toml -- analyze <path to repo>
```

Running
-------

First, run `git-of-theseus-analyze <path to repo>` (see `git-of-theseus-analyze --help` for configuration options). This analyzes a repository and may take some time.

After that, you can generate plots! Some examples:

1. Run `git-of-theseus-stack-plot cohorts.json` to create a stack plot showing total code broken down into cohorts (what year code was added)
1. Run `git-of-theseus-line-plot authors.json --normalize` to show the % of code contributed by top authors
1. Run `git-of-theseus-survival-plot survival.json`

Run any command with `--help` to see its available options.

If you want to plot multiple repositories, have to run `git-of-theseus-analyze` separately for each project and store the data in separate directories using the `--outdir` flag. Then you can run `git-of-theseus-survival-plot <foo/survival.json> <bar/survival.json>` (optionally with the `--exp-fit` flag to fit an exponential decay)

Help
----

- `git-of-theseus-analyze --help`
- `git-of-theseus-stack-plot --help`
- `git-of-theseus-line-plot --help`
- `git-of-theseus-survival-plot --help`

  
Some pics
---------

Survival of a line of code in a set of interesting repos:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-projects-survival.png)

This curve is produced by the `git-of-theseus-survival-plot` script and shows the *percentage of lines in a commit that are still present after x years*. It aggregates it over all commits, no matter what point in time they were made. So for *x=0* it includes all commits, whereas for *x>0* not all commits are counted (because we would have to look into the future for some of them). The survival curves are estimated using [Kaplan-Meier](https://en.wikipedia.org/wiki/Kaplan%E2%80%93Meier_estimator).

You can also add an exponential fit:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-projects-survival-exp-fit.png)

Linux – stack plot:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-linux.png)

This curve is produced by the `git-of-theseus-stack-plot` script and shows the total number of lines in a repo broken down into cohorts by the year the code was added.

Node – stack plot:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-node.png)

Rails – stack plot:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-rails.png)

Tensorflow – stack plot:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-tensorflow.png)

Rust – stack plot:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-rust.png)

Plotting other stuff
--------------------

`git-of-theseus-analyze` will write `exts.json`, `cohorts.json` and `authors.json`. You can run `git-of-theseus-stack-plot authors.json` to plot author statistics as well, or `git-of-theseus-stack-plot exts.json` to plot file extension statistics. For author statistics, you might want to create a [.mailmap](https://git-scm.com/docs/gitmailmap) file in the root directory of the repository to deduplicate authors. If you need to create a .mailmap file the following command can list the distinct author-email combinations in a repository:

Mac / Linux

```shell
git log --pretty=format:"%an %ae" | sort | uniq
```

Windows Powershell

```powershell
git log --pretty=format:"%an %ae" | Sort-Object | Select-Object -Unique
```

For instance, here's the author statistics for [Kubernetes](https://github.com/kubernetes/kubernetes):

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-kubernetes-authors.png)

You can also normalize it to 100%. Here's author statistics for Git:

![git](https://raw.githubusercontent.com/erikbern/git-of-theseus/master/pics/git-git-authors-normalized.png)

Other stuff
-----------

[Markovtsev Vadim](https://twitter.com/tmarkhor) implemented a very similar analysis that claims to be 20%-6x faster than Git of Theseus. It's named [Hercules](https://github.com/src-d/hercules) and there's a great [blog post](https://web.archive.org/web/20180918135417/https://blog.sourced.tech/post/hercules.v4/) about all the complexity going into the analysis of Git history.
