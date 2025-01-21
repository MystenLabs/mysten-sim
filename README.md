# The Mysten Simulator

**This is a fork of https://github.com/madsim-rs/madsim**

The madsim project is building a tokio-like (but not 100% compatible) deterministic runtime.
This fork modifies the original project to produce a drop-in replacement for tokio.

## How to upgrade the tokio version:

1. Clone the tokio fork, add an upstream remote, and fetch

        $ git clone git@github.com:MystenLabs/tokio-msim-fork.git
        $ cd tokio-msim-fork
        $ git remote add upstream git@github.com:tokio-rs/tokio.git
        $ git fetch upstream --tags

2. See what our current version of tokio is - don't just look in Cargo.toml since we can be ahead of the version requested there.

        # cd sui
        # cargo tree -i tokio --depth 0

2. Now rebase the fork onto the tokio release you want to use.

        # make a new branch for the version we want to upgrade TO
        # start the branch from the current version, that we are upgrading FROM
        $ git checkout -b msim-1.43.0 msim-1.38.1

        # and rebase using the version tag
        $ git rebase tokio-1.43.0

        # push the rebased version to our repo and remember the current commit
        $ git push
        $ FORK_COMMIT=`git rev-parse HEAD`

3. Usually, there are no merge conflicts. If there are, the scope of the fork is limited and things should be fixable.

4. Now, in this repo

    1. Edit the version in `msim-tokio/Cargo.toml` to match the version we are upgrading to (`1.43.0` in this example).
    2. Find all references to `https://github.com/MystenLabs/tokio-msim-fork.git` in Cargo.toml files in this repo and update the `rev` param to `FORK_COMMIT`.

5. Now, in the sui repo, update the tokio version by editing Cargo.toml, or by running:

        $ cargo update -p tokio --precise 1.43.0

6. Install simtest (if you already have it, skip this step): https://github.com/MystenLabs/sui/blob/main/scripts/simtest/install.sh

7. Test all the changes against your local msim repo - if there are build errors the rebasing may have gone wrong.
        $ cd sui
        $ LOCAL_MSIM_PATH=/path/to/mysten-sim/msim cargo simtest


## Usage:

TODO
