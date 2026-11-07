#pragma once

#include <cstring>

// Whether `qveil <arguments>` opens the window: `qveil` alone and `qveil open`
// alone do. Anything more is a command for the engine, `open` included
// (`qveil open --help` asks the engine for help; it doesn't open a window
// that the arguments then silently ignore). argv[0] is the program itself.
inline bool opensWindow(int argc, const char *const *argv) {
    return argc <= 1 || (argc == 2 && std::strcmp(argv[1], "open") == 0);
}
