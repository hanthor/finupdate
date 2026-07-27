# Desktop file — rename to match the application_id for Flatpak export.
desktop_conf = configuration_data()
desktop_conf.set('APP_ID', application_id)
desktop_conf.set('ICON', application_id)

desktop_file = configure_file(
  input: 'org.tunaos.finupdate.desktop.in',
  output: application_id + '.desktop',
  configuration: desktop_conf,
)

install_data(
  desktop_file,
  install_dir: datadir / 'applications'
)

if desktop_file_validate.found()
  test(
    'validate-desktop',
    desktop_file_validate,
    args: [ desktop_file ],
  )
endif

# AppStream metainfo — rename to match application_id.
metainfo_conf = configuration_data()
metainfo_conf.set('APP_ID', application_id)

metainfo_file = configure_file(
  input: 'org.tunaos.finupdate.metainfo.xml.in',
  output: application_id + '.metainfo.xml',
  configuration: metainfo_conf,
)

install_data(
  metainfo_file,
  install_dir: datadir / 'metainfo'
)

if appstreamcli.found()
  test(
    'validate-metainfo',
    appstreamcli,
    args: [ 'validate', '--no-net', '--explain', metainfo_file ],
  )
endif

# Icons — install with application_id name so Flatpak export picks them up.
install_data(
  'icons' / 'org.tunaos.finupdate.svg',
  install_dir: iconsdir / 'hicolor' / 'scalable' / 'apps',
  rename: application_id + '.svg',
)

install_data(
  'icons' / 'org.tunaos.finupdate-symbolic.svg',
  install_dir: iconsdir / 'hicolor' / 'symbolic' / 'apps',
  rename: application_id + '-symbolic.svg',
)

# Runner script — invoked as root via a single pkexec elevation.
# Installed to /app/bin/ so the Flatpak sandbox can find it.
install_data(
  'finupdate-runner',
  install_dir: bindir,
  install_mode: 'rwxr-xr-x',
)

# GSettings schema — replaces the hand-rolled settings.json (HIG spec §9.1).
# Templated on application_id so the Devel profile gets its own schema id and
# path, and the two builds cannot clobber one another's configuration.
gschema_conf = configuration_data()
gschema_conf.set('APP_ID', application_id)
# The path has to be derived too, not just the id. It was hardcoded while the
# id was templated, so the Devel and release schemas shared one dconf path and
# each build read the other's configuration — the exact clobbering the comment
# above says this templating prevents.
gschema_conf.set('APP_PATH', '/' + application_id.replace('.', '/') + '/')

gschema_file = configure_file(
  input: 'org.tunaos.finupdate.gschema.xml.in',
  output: application_id + '.gschema.xml',
  configuration: gschema_conf,
)

install_data(
  gschema_file,
  install_dir: datadir / 'glib-2.0' / 'schemas',
)

# Compile schemas at install time so the app can find them without a manual
# glib-compile-schemas step.
gnome.post_install(glib_compile_schemas: true)

compile_schemas = find_program('glib-compile-schemas', required: false)
if compile_schemas.found()
  test(
    'validate-gschema',
    compile_schemas,
    args: ['--strict', '--dry-run', meson.current_build_dir()],
  )
endif
