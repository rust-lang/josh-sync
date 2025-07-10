# Josh sync utilities
This repository contains a binary utility for performing [Josh](https://github.com/josh-project/josh)
synchronizations (pull and push) of Josh subtrees in the [rust-lang/rust] repository.

## Installation
You can install the binary `rustc-josh-sync` tool using the following command:

```bash
$ cargo install --locked --git https://github.com/rust-lang/josh-sync
```

## Creating config file

First, create a configuration file for a given subtree repo using `rustc-josh-sync init`. The config will be created under the path `josh-sync.toml`. Modify the file to fill in the name of the subtree repository (e.g. `stdarch`) and its relative path in the main `rust-lang/rust` repository (e.g. `library/stdarch`).

If you need to specify a more complex Josh `filter`, use `filter` field in the configuration file instead of the `path` field.

The `init` command will also create an empty `rust-version` file (if it doesn't already exist) that stores the last upstream `rustc` SHA that was synced in the subtree.

## Performing pull

A pull operation fetches changes to the subtree subdirectory that were performed in `rust-lang/rust` and merges them into the subtree repository. After performing a pull, a pull request is sent against the *subtree repository*. We *pull from rustc*.

1) Checkout the latest default branch of the subtree
2) Create a new branch that will be used for the subtree PR, e.g. `pull`
3) Run `rustc-josh-sync pull`
4) Send a PR to the subtree repository
    - Note that `rustc-josh-sync` can do this for you if you have the [gh](https://cli.github.com/) CLI tool installed.

## Performing push

A push operation takes changes performed in the subtree repository and merges them into the subtree subdirectory of the `rust-lang/rust` repository. After performing a push, a push request is sent against the *rustc repository*. We *push to rustc*.

1) Checkout the latest default branch of the subtree
2) Run `rustc-josh-sync pull <your-github-username> <branch>`
    - The branch with the push contents will be created in `https://github.com/<your-github-username>/rust` fork, in the `<branch>` branch.
3) Send a PR to [rust-lang/rust]

## Automating pulls on CI

This repository contains a reusable workflow for performing the `pull` operation from CI. The workflow does the following:

1) Installs `rustc-josh-sync` and `josh`
2) Performs a `pull` operation
3) Either creates a new PR (if it did not exist) with the resulting pull branch or force-pushes to an existing PR on the subtree repository
4) (optional) If a failure (usually a merge conflict) has happened, or a PR has been opened for more than a week without a merge, it posts a message to a Zulip stream

Here is an example of how you can use the workflow in a subtree repository:

```yaml
name: rustc-pull

on:
  workflow_dispatch:
  schedule:
    # Run at 04:00 UTC every Monday and Thursday
    - cron: '0 4 * * 1,4'

jobs:
  pull:
    uses: rust-lang/josh-sync/.github/workflows/rustc-pull.yml@main
    with:
      # If you want the Zulip post functionality
      #zulip-stream-id: 1234   # optional
      #zulip-bot-email: subtree-gha-notif-bot@rust-lang.zulipchat.com # optional
      pr-base-branch: master   # optional
      branch-name: rustc-pull  # optional
    secrets:
      #zulip-api-token: <Zulip API TOKEN>     # optional
      token: ${{ secrets.GITHUB_TOKEN }}
```

## Git peculiarities

NOTE: If you use Git/SSH protocol to push to your fork of [rust-lang/rust],
ensure that you have this entry in your Git config,
else the 2 steps that follow would prompt for a username and password:

```
[url "git@github.com:"]
insteadOf = "https://github.com/"
```

### Minimal git config

For simplicity (ease of implementation purposes), the josh-sync script simply calls out to system git. This means that the git invocation may be influenced by global (or local) git configuration.

You may observe "Nothing to pull" even if you *know* rustc-pull has something to pull if your global git config sets `fetch.prunetags = true` (and possibly other configurations may cause unexpected outcomes).

To minimize the likelihood of this happening, you may wish to keep a separate *minimal* git config that *only* has `[user]` entries from global git config, then repoint system git to use the minimal git config instead. E.g.

```
GIT_CONFIG_GLOBAL=/path/to/minimal/gitconfig GIT_CONFIG_SYSTEM='' rustc-josh-sync ...
```

[rust-lang/rust]: (https://github.com/rust-lang/rust)
