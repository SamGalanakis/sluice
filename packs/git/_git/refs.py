"""The check every git/gh fn runs on a plan-supplied ref before it reaches the command line."""


def ref(name, value):
    """A plan-supplied ref, branch or remote may not start with '-': positionally it would be
    read as an option (`git push --all`, `gh pr view --repo=other/x`)."""
    if str(value).startswith("-"):
        raise ValueError(f"{name} may not start with '-': {value}")
    return value
