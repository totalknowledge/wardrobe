#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: utilities/scripts/update-version.sh [--date YYYY-MM-DD] [--dry-run]"
}

calendar_date=""
dry_run=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --date)
            if [[ $# -lt 2 ]]; then
                echo "update-version: --date requires YYYY-MM-DD" >&2
                exit 2
            fi
            calendar_date="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "update-version: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "$calendar_date" ]]; then
    calendar_date="$(date +%Y-%m-%d)"
fi

if [[ ! "$calendar_date" =~ ^([0-9]{4})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$ ]]; then
    echo "update-version: invalid date '$calendar_date'; expected YYYY-MM-DD" >&2
    exit 2
fi

year="${BASH_REMATCH[1]}"
month="$((10#${BASH_REMATCH[2]}))"
day="${BASH_REMATCH[3]}"
release_year="${year:2:2}"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "$script_dir/../.." && pwd -P)"
core_manifest="$repo_root/core/Cargo.toml"

current_version="$(
    sed -n 's/^version = "\([^"]*\)"$/\1/p' "$core_manifest" | head -n 1
)"

if [[ ! "$current_version" =~ ^([0-9]+)\.[0-9]{2}\.[0-9]{3,4}$ ]]; then
    echo "update-version: unsupported current version '$current_version' in core/Cargo.toml" >&2
    exit 1
fi

major="${BASH_REMATCH[1]}"
next_version="${major}.${release_year}.${month}${day}"

if [[ "$current_version" == "$next_version" ]]; then
    echo "Version is already $next_version."
    exit 0
fi

cd "$repo_root"
mapfile -d '' -t version_files < <(
    git ls-files --cached --others --exclude-standard -z |
        OLD_VERSION="$current_version" perl -0ne '
            chomp;
            next if m{\Auser_stories/};
            next unless -f;
            open my $file, "<", $_ or die "cannot read $_: $!\n";
            binmode $file;
            local $/;
            my $contents = <$file>;
            close $file;
            next if !defined $contents || index($contents, "\0") >= 0;
            push @matches, $_ if index($contents, $ENV{OLD_VERSION}) >= 0;
            END { print "$_\0" for sort @matches; }
        '
)

if [[ ${#version_files[@]} -eq 0 ]]; then
    echo "update-version: no project files contain $current_version" >&2
    exit 1
fi

if [[ "$dry_run" == true ]]; then
    echo "Would update $current_version to $next_version in ${#version_files[@]} files:"
    printf '  %s\n' "${version_files[@]#./}"
    exit 0
fi

OLD_VERSION="$current_version" NEW_VERSION="$next_version" \
    perl -0pi -e 's/\Q$ENV{OLD_VERSION}\E/$ENV{NEW_VERSION}/g' -- "${version_files[@]}"

for version_file in "${version_files[@]}"; do
    if grep -Fq -- "$current_version" "$version_file"; then
        echo "update-version: failed to update ${version_file#./}" >&2
        exit 1
    fi
done

echo "Updated $current_version to $next_version in ${#version_files[@]} files."
