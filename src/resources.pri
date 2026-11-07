# The resources, shared by the app and its tests.
RESOURCES += $$PWD/resources.qrc

# The veil's shader is compiled into the build directory, never into the
# source tree, for every GL version the scene graph may use.
SHADER_SOURCES = $$PWD/shaders/dye.frag
qsb.input = SHADER_SOURCES
qsb.output = ${QMAKE_FILE_IN_BASE}.frag.qsb
qsb.commands = $$[QT_INSTALL_BINS]/qsb --glsl \"100es,120,150\" -o ${QMAKE_FILE_OUT} ${QMAKE_FILE_IN}
qsb.CONFIG += no_link target_predeps
QMAKE_EXTRA_COMPILERS += qsb

# A resource file cannot name a file by a path that depends on where the build
# directory is, so one is written into it, next to the shader it names.
SHADER_QRC_LINES = \
    "<RCC>" \
    "    <qresource prefix=\"/\">" \
    "        <file alias=\"shaders/dye.frag.qsb\">dye.frag.qsb</file>" \
    "    </qresource>" \
    "</RCC>"
write_file($$OUT_PWD/shaders.qrc, SHADER_QRC_LINES)
RESOURCES += $$OUT_PWD/shaders.qrc

# rcc finds the shader's name in that file, but when qmake runs the shader
# doesn't exist yet, so the dependency has to be spelled out for make.
shader_resource.target = qrc_shaders.cpp
shader_resource.depends = dye.frag.qsb
QMAKE_EXTRA_TARGETS += shader_resource
