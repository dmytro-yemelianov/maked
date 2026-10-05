#compdef makeyd

# Zsh completion for makeyd (POSIX/GNU compatible Make with formal Lean 4 semantics)

_makeyd() {
    local curcontext="$curcontext" state line
    typeset -A opt_args

    local -a common_args
    common_args=(
        '(-f)'{-f}+'[Read specified FILE as makefile]:makefile:_files'
        '(-j)'{-j}+'[Allow N jobs at once]:jobs:(1 2 4 8 16 32)'
        '(-n --dry-run)'{-n,--dry-run}"[Don't actually run recipes; just print them]"
        '(-B --always-make)'{-B,--always-make}'[Unconditionally remake all targets]'
        '(-s --silent --quiet)'{-s,--silent,--quiet}"[Don't echo recipe commands]"
        '(-q --question)'{-q,--question}'[Question mode: return exit 0 if up to date, 1 otherwise]'
        '(-C)'{-C}+'[Change to DIR before doing anything]:directory:_files -/'
        '--profile[Display execution and profiling statistics]'
        '--hash[Use SHA-256 cryptographic content hashes instead of timestamps]'
        '(-e --environment-overrides)'{-e,--environment-overrides}'[Environment variables override makefile assignments]'
        '(-i --ignore-errors)'{-i,--ignore-errors}'[Ignore all errors in recipes]'
        '(-k --keep-going)'{-k,--keep-going}'[Keep going on errors as much as possible]'
        '(-p --print-data-base)'{-p,--print-data-base}'[Print database of rules and variables]'
        '(-t --touch)'{-t,--touch}'[Touch targets instead of remaking them]'
        '(-v --version)'{-v,--version}'[Print version information and exit]'
        '(-h --help)'{-h,--help}'[Print help message and exit]'
        '*:target:->targets'
    )

    _arguments -s $common_args && return 0

    case "$state" in
        targets)
            local makefile="Makefile"
            local dir="."
            if [[ -n "$opt_args[-C]" ]]; then
                dir="$opt_args[-C]"
            fi
            if [[ -n "$opt_args[-f]" ]]; then
                makefile="$opt_args[-f]"
            fi

            local target_file="$dir/$makefile"
            if [[ ! -f "$target_file" && -f "$dir/makefile" ]]; then
                target_file="$dir/makefile"
            fi

            if [[ -f "$target_file" ]]; then
                local -a targets
                targets=(${(f)"$(grep -E '^[a-zA-Z0-9_.-]+[[:space:]]*:' "$target_file" 2>/dev/null | \
                                grep -v '^\.PHONY' | grep -v '^\.SUFFIXES' | \
                                sed 's/:.*//')"})
                _describe 'target' targets
            fi
            ;;
    esac
}

_makeyd "$@"
