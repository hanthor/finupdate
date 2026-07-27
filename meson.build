project(
  'finupdate',
  'rust',
  version: '0.1.0',
  license: 'MIT',
  meson_version: '>= 0.59',
)

i18n = import('i18n')
gnome = import('gnome')

base_id = 'org.tunaos.finupdate'

dependency('glib-2.0', version: '>= 2.80')
dependency('gio-2.0', version: '>= 2.80')
dependency('gtk4', version: '>= 4.16')
dependency('libadwaita-1', version: '>= 1.7')

cargo = find_program('cargo', required: true)
desktop_file_validate = find_program('desktop-file-validate', required: false)
appstreamcli = find_program('appstreamcli', required: false)

version = meson.project_version()

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
localedir = prefix / get_option('localedir')

datadir = prefix / get_option('datadir')
pkgdatadir = datadir / meson.project_name()
iconsdir = datadir / 'icons'

if get_option('profile') == 'development'
  profile = 'Devel'
  application_id = '@0@.Devel'.format(base_id)
else
  profile = ''
  application_id = base_id
endif

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.project_build_root() / 'meson-dist' / meson.project_name() + '-' + version,
  meson.project_source_root()
)

subdir('data')
subdir('src')

gnome.post_install(
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)
