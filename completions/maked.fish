# Fish completion for maked (POSIX/GNU compatible Make with formal Lean 4 semantics)

function __fish_maked_targets
    set -l makefile "Makefile"
    set -l dir "."

    set -l cmd (commandline -poc)
    set -l skip_next 0

    for i in (seq (count $cmd))
        if test $skip_next -eq 1
            set skip_next 0
            continue
        end
        switch $cmd[$i]
            case '-C'
                set dir $cmd[(math $i + 1)]
                set skip_next 1
            case '-f'
                set makefile $cmd[(math $i + 1)]
                set skip_next 1
        end
    end

    set -l target_file "$dir/$makefile"
    if not test -f "$target_file"; and test -f "$dir/makefile"
        set target_file "$dir/makefile"
    end

    if test -f "$target_file"
        grep -E '^[a-zA-Z0-9_.-]+[[:space:]]*:' "$target_file" 2>/dev/null | \
            grep -v '^\.PHONY' | grep -v '^\.SUFFIXES' | \
            sed 's/:.*//'
    end
end

complete -c maked -s f -r -d "Read FILE as a makefile"
complete -c maked -s j -x -d "Allow N jobs at once"
complete -c maked -s n -l dry-run -d "Don't actually run recipes; just print them"
complete -c maked -s B -l always-make -d "Unconditionally remake all targets"
complete -c maked -s s -l silent -l quiet -d "Don't echo recipe commands"
complete -c maked -s q -l question -d "Question mode: return exit 0 if up to date, 1 otherwise"
complete -c maked -s C -r -d "Change to DIR before doing anything"
complete -c maked -l profile -d "Display execution and profiling statistics"
complete -c maked -l hash -d "Use SHA-256 cryptographic content hashes instead of timestamps"
complete -c maked -s e -l environment-overrides -d "Environment variables override makefile assignments"
complete -c maked -s i -l ignore-errors -d "Ignore all errors in recipes"
complete -c maked -s k -l keep-going -d "Keep going on errors as much as possible"
complete -c maked -s p -l print-data-base -d "Print database of rules and variables"
complete -c maked -s t -l touch -d "Touch targets instead of remaking them"
complete -c maked -s v -l version -d "Print version information and exit"
complete -c maked -s h -l help -d "Print help message and exit"

complete -c maked -f -a "(__fish_maked_targets)" -d "Build target"
