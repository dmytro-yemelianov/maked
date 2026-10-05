# Bash completion for maked (POSIX/GNU compatible Make with formal Lean 4 semantics)

_maked_completions() {
    local cur prev words cword
    _init_completion -n : || return

    local options="-f -j -n --dry-run -B --always-make -s --silent --quiet -q --question -C --profile --hash -e --environment-overrides -i --ignore-errors -k --keep-going -p --print-data-base -t --touch -v --version -h --help"

    case "$prev" in
        -f)
            _filedir
            return 0
            ;;
        -C)
            _filedir -d
            return 0
            ;;
        -j)
            COMPREPLY=( $(compgen -W "1 2 4 8 16 32" -- "$cur") )
            return 0
            ;;
    esac

    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "$options" -- "$cur") )
        return 0
    fi

    # Find Makefile in current directory or -C directory
    local makefile="Makefile"
    local dir="."
    local i
    for (( i=1; i < ${#words[@]}; i++ )); do
        if [[ "${words[i]}" == "-C" && $((i+1)) -lt ${#words[@]} ]]; then
            dir="${words[i+1]}"
        elif [[ "${words[i]}" == "-f" && $((i+1)) -lt ${#words[@]} ]]; then
            makefile="${words[i+1]}"
        fi
    done

    local target_file="$dir/$makefile"
    if [[ ! -f "$target_file" && -f "$dir/makefile" ]]; then
        target_file="$dir/makefile"
    fi

    if [[ -f "$target_file" ]]; then
        # Extract targets: lines starting with target name followed by ':'
        local targets=$(grep -E '^[a-zA-Z0-9_.-]+[[:space:]]*:' "$target_file" 2>/dev/null | \
                        grep -v '^\.PHONY' | grep -v '^\.SUFFIXES' | \
                        sed 's/:.*//' | tr '\n' ' ')
        COMPREPLY=( $(compgen -W "$targets" -- "$cur") )
        return 0
    fi
}

complete -F _maked_completions maked
