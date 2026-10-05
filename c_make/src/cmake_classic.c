#define _POSIX_C_SOURCE 200809L
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>
#include <ctype.h>
#include <stdbool.h>

#define MAX_LINE 16384
#define MAX_TARGETS 4096
#define MAX_PREREQS 2048
#define MAX_COMMANDS 128
#define MAX_VARS 1024

/* Build states corresponding to Feldman's doname.c */
enum TargetState {
    STATE_NOT_DONE = 0,
    STATE_WORKING = 1,
    STATE_DONE = 2,
    STATE_FAILED = 3
};

typedef struct {
    char *name;
    char *value;
} Variable;

typedef struct Target {
    char *name;
    char *prereqs[MAX_PREREQS];
    int prereq_count;
    char *commands[MAX_COMMANDS];
    int command_count;
    enum TargetState state;
    time_t modtime;
    bool is_phony;
} Target;

static Target targets[MAX_TARGETS];
static int target_count = 0;
static Variable variables[MAX_VARS];
static int var_count = 0;

/* Flags */
static bool flag_dry_run = false;      /* -n */
static bool flag_always_make = false;  /* -B */
static bool flag_silent = false;       /* -s */
static bool flag_question = false;     /* -q */

/* Variable management */
static void set_var(const char *name, const char *val) {
    for (int i = 0; i < var_count; i++) {
        if (strcmp(variables[i].name, name) == 0) {
            free(variables[i].value);
            variables[i].value = strdup(val);
            return;
        }
    }
    if (var_count < MAX_VARS) {
        variables[var_count].name = strdup(name);
        variables[var_count].value = strdup(val);
        var_count++;
    }
}

static const char *get_var(const char *name) {
    for (int i = 0; i < var_count; i++) {
        if (strcmp(variables[i].name, name) == 0) {
            return variables[i].value;
        }
    }
    const char *env = getenv(name);
    return env ? env : "";
}

/* Expand variables: $(VAR) or ${VAR} */
static char *expand_vars(const char *src) {
    char buf[MAX_LINE * 2];
    size_t bi = 0;
    size_t len = strlen(src);
    for (size_t i = 0; i < len && bi < sizeof(buf) - 1; ) {
        if (src[i] == '$' && i + 1 < len) {
            if (src[i + 1] == '$') {
                buf[bi++] = '$';
                i += 2;
                continue;
            }
            char closing = 0;
            size_t start = 0;
            if (src[i + 1] == '(') { closing = ')'; start = i + 2; }
            else if (src[i + 1] == '{') { closing = '}'; start = i + 2; }
            if (closing) {
                size_t end = start;
                while (end < len && src[end] != closing) end++;
                if (end < len) {
                    char var_name[256];
                    size_t vlen = end - start;
                    if (vlen >= sizeof(var_name)) vlen = sizeof(var_name) - 1;
                    strncpy(var_name, src + start, vlen);
                    var_name[vlen] = '\0';
                    const char *val = get_var(var_name);
                    size_t vlen2 = strlen(val);
                    for (size_t k = 0; k < vlen2 && bi < sizeof(buf) - 1; k++) {
                        buf[bi++] = val[k];
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        buf[bi++] = src[i++];
    }
    buf[bi] = '\0';
    return strdup(buf);
}

static Target *find_or_create_target(const char *name) {
    for (int i = 0; i < target_count; i++) {
        if (strcmp(targets[i].name, name) == 0) {
            return &targets[i];
        }
    }
    if (target_count >= MAX_TARGETS) {
        fprintf(stderr, "make: too many targets (max %d)\n", MAX_TARGETS);
        exit(1);
    }
    Target *t = &targets[target_count++];
    memset(t, 0, sizeof(*t));
    t->name = strdup(name);
    t->state = STATE_NOT_DONE;
    return t;
}

/* Get file modification time. Returns 0 if file does not exist. */
static time_t get_mtime(const char *path) {
    struct stat st;
    if (stat(path, &st) == 0) {
        return st.st_mtime;
    }
    return 0;
}

/* Execute a single shell command */
static int exec_command(const char *cmd) {
    bool is_silent = flag_silent;
    bool ignore_err = false;
    const char *p = cmd;
    while (*p == ' ' || *p == '\t') p++;
    while (*p == '@' || *p == '-') {
        if (*p == '@') is_silent = true;
        if (*p == '-') ignore_err = true;
        p++;
        while (*p == ' ' || *p == '\t') p++;
    }

    if (!is_silent) {
        printf("%s\n", p);
        fflush(stdout);
    }
    if (flag_dry_run) {
        return 0;
    }
    int ret = system(p);
    if (ret != 0 && !ignore_err) {
        fprintf(stderr, "make: *** Command failed with exit code %d: %s\n", ret, p);
        return -1;
    }
    return 0;
}

/*
 * Classic Feldman doname() recursive DAG traversal algorithm.
 * Returns:
 *   0: target is up to date (no work needed or work succeeded)
 *  -1: error / build failure
 *   1: work was performed
 */
static int doname(Target *t, int depth) {
    if (t == NULL) return 0;

    /* Cycle detection check: if STATE_WORKING, we have a cycle */
    if (t->state == STATE_WORKING) {
        /* Classic make: either drops or loops. We flag circular dependency. */
        fprintf(stderr, "make: warning: circular dependency on target '%s' dropped\n", t->name);
        return 0;
    }

    if (t->state == STATE_DONE) {
        return 0;
    }
    if (t->state == STATE_FAILED) {
        return -1;
    }

    t->state = STATE_WORKING;

    time_t target_mtime = 0;
    bool target_exists = false;

    if (!t->is_phony) {
        target_mtime = get_mtime(t->name);
        target_exists = (target_mtime != 0);
        t->modtime = target_mtime;
    }

    bool need_rebuild = flag_always_make || !target_exists || t->is_phony;
    time_t newest_prereq_mtime = 0;

    /* Process all prerequisites recursively */
    for (int i = 0; i < t->prereq_count; i++) {
        Target *dep = find_or_create_target(t->prereqs[i]);
        int res = doname(dep, depth + 1);
        if (res < 0) {
            t->state = STATE_FAILED;
            return -1;
        }

        time_t dep_mtime = dep->is_phony ? 0 : get_mtime(dep->name);
        if (dep_mtime > newest_prereq_mtime) {
            newest_prereq_mtime = dep_mtime;
        }

        /* If prerequisite is newer than target, target needs rebuild */
        if (!dep->is_phony && target_exists && dep_mtime > target_mtime) {
            need_rebuild = true;
        }
        if (res == 1) {
            /* If a prerequisite was rebuilt, we need to rebuild */
            need_rebuild = true;
        }
    }

    int result = 0;

    if (need_rebuild) {
        if (flag_question) {
            t->state = STATE_DONE;
            return 1;
        }

        /* Execute commands */
        for (int i = 0; i < t->command_count; i++) {
            char *expanded_cmd = expand_vars(t->commands[i]);
            int rc = exec_command(expanded_cmd);
            free(expanded_cmd);
            if (rc != 0) {
                t->state = STATE_FAILED;
                return -1;
            }
        }

        /* Re-stat after commands ran */
        if (!t->is_phony) {
            t->modtime = get_mtime(t->name);
        }
        result = 1; /* work was done */
    }

    t->state = STATE_DONE;
    return result;
}

/* Parse a simple Makefile */
static void parse_makefile(const char *filename) {
    FILE *fp = fopen(filename, "r");
    if (!fp) {
        perror(filename);
        exit(2);
    }

    char line[MAX_LINE];
    Target *current_target = NULL;

    while (fgets(line, sizeof(line), fp)) {
        /* Strip trailing newline / carriage return */
        size_t len = strlen(line);
        while (len > 0 && (line[len - 1] == '\n' || line[len - 1] == '\r')) {
            line[--len] = '\0';
        }

        /* Skip empty lines and comments */
        if (len == 0 || line[0] == '#') continue;

        /* Check for recipe line (indented by tab) */
        if (line[0] == '\t') {
            if (!current_target) {
                fprintf(stderr, "%s: recipe commences before first target\n", filename);
                exit(2);
            }
            char *cmd = line + 1;
            while (*cmd == ' ' || *cmd == '\t') cmd++;
            if (*cmd != '\0' && current_target->command_count < MAX_COMMANDS) {
                current_target->commands[current_target->command_count++] = strdup(cmd);
            }
            continue;
        }

        /* Variable assignment: VAR = VAL or VAR := VAL */
        char *eq = strchr(line, '=');
        char *colon = strchr(line, ':');

        if (eq && (!colon || eq < colon)) {
            *eq = '\0';
            char *var_name = line;
            while (*var_name == ' ') var_name++;
            char *end_name = eq - 1;
            if (end_name >= var_name && *end_name == ':') end_name--; /* := */
            while (end_name >= var_name && *end_name == ' ') *end_name-- = '\0';
            *(end_name + 1) = '\0';

            char *var_val = eq + 1;
            while (*var_val == ' ') var_val++;
            set_var(var_name, var_val);
            current_target = NULL;
            continue;
        }

        /* Target line: target: prereq1 prereq2 ... */
        if (colon) {
            *colon = '\0';
            char *target_name = line;
            while (*target_name == ' ') target_name++;
            char *end_target = colon - 1;
            while (end_target >= target_name && *end_target == ' ') *end_target-- = '\0';
            *(end_target + 1) = '\0';

            char *prereqs_str = colon + 1;
            while (*prereqs_str == ' ') prereqs_str++;

            /* Handle .PHONY */
            if (strcmp(target_name, ".PHONY") == 0) {
                char *token = strtok(prereqs_str, " \t");
                while (token) {
                    Target *pt = find_or_create_target(token);
                    pt->is_phony = true;
                    token = strtok(NULL, " \t");
                }
                current_target = NULL;
                continue;
            }

            Target *t = find_or_create_target(target_name);
            current_target = t;

            char *token = strtok(prereqs_str, " \t");
            while (token) {
                if (t->prereq_count < MAX_PREREQS) {
                    t->prereqs[t->prereq_count++] = strdup(token);
                }
                token = strtok(NULL, " \t");
            }
        }
    }

    fclose(fp);
}

int main(int argc, char **argv) {
    const char *makefile_path = "Makefile";
    const char *target_name = NULL;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-f") == 0 && i + 1 < argc) {
            makefile_path = argv[++i];
        } else if (strcmp(argv[i], "-n") == 0) {
            flag_dry_run = true;
        } else if (strcmp(argv[i], "-B") == 0) {
            flag_always_make = true;
        } else if (strcmp(argv[i], "-s") == 0) {
            flag_silent = true;
        } else if (strcmp(argv[i], "-q") == 0) {
            flag_question = true;
        } else if (strcmp(argv[i], "-C") == 0 && i + 1 < argc) {
            if (chdir(argv[++i]) != 0) {
                perror("chdir");
                exit(2);
            }
        } else if (argv[i][0] != '-') {
            target_name = argv[i];
        }
    }

    /* Check if Makefile or makefile exists */
    if (access(makefile_path, R_OK) != 0) {
        if (strcmp(makefile_path, "Makefile") == 0 && access("makefile", R_OK) == 0) {
            makefile_path = "makefile";
        } else {
            fprintf(stderr, "make: *** No targets specified and no makefile found.  Stop.\n");
            return 2;
        }
    }

    parse_makefile(makefile_path);

    if (target_count == 0) {
        fprintf(stderr, "make: *** No targets.  Stop.\n");
        return 2;
    }

    Target *root = NULL;
    if (target_name) {
        for (int i = 0; i < target_count; i++) {
            if (strcmp(targets[i].name, target_name) == 0) {
                root = &targets[i];
                break;
            }
        }
        if (!root) {
            fprintf(stderr, "make: *** No rule to make target '%s'.  Stop.\n", target_name);
            return 2;
        }
    } else {
        /* Default to first non-phony target */
        for (int i = 0; i < target_count; i++) {
            if (!targets[i].is_phony) {
                root = &targets[i];
                break;
            }
        }
        if (!root) root = &targets[0];
    }

    int rc = doname(root, 0);
    if (rc < 0) {
        return 1;
    }
    if (rc == 0 && !flag_question) {
        if (!flag_silent) {
            printf("make: '%s' is up to date.\n", root->name);
        }
    }
    return (flag_question && rc == 1) ? 1 : 0;
}
