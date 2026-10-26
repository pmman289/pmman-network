"""Release rollback tests. Docker integration uses a disposable local database."""
import importlib.util
import json
import os
import pathlib
import subprocess
import tempfile
import time
import unittest
import uuid
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'activate_server_release', pathlib.Path(__file__).with_name('activate-server-release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class RollbackTests(unittest.TestCase):
    def test_missing_backup_does_not_modify_database(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(release, 'run') as run:
            release.restore_postgres(pathlib.Path(directory), 'pmman', 'pmman')
            run.assert_not_called()

    def test_only_new_tables_are_removed_with_quoted_identifiers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / 'postgres.sql').write_bytes(b'-- fixture dump\n')
            (root / 'postgres-tables.json').write_text(json.dumps(['original']))
            with patch.object(release, 'public_tables', return_value=['original', 'new"table']), \
                    patch.object(release, 'run') as run, \
                    patch.object(release.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)):
                release.restore_postgres(root, 'pmman', 'pmman')
            calls = [call.args for call in run.call_args_list]
            self.assertEqual(calls[0][-2:], ('stop', 'master'))
            self.assertEqual(calls[1][-1], 'DROP TABLE public."new""table" CASCADE')

    def test_upgrade_quiesces_master_before_backup(self):
        source = pathlib.Path(release.__file__).read_text()
        main = source[source.index('def main():'):]
        self.assertLess(main.index("'stop', 'master'"), main.index('backup_postgres(previous'))
        self.assertLess(main.index('backup_postgres(previous'), main.index('restart_units()'))


@unittest.skipUnless(os.environ.get('PMMAN_RELEASE_DOCKER_TEST') == '1', 'requires local Docker')
class DockerRollbackTests(unittest.TestCase):
    def test_restore_removes_new_schema_and_restores_original_rows(self):
        container = 'pmman-release-test-' + uuid.uuid4().hex[:12]
        real_run = subprocess.run

        def docker(*args, **kwargs):
            return real_run(['docker', *args], check=True, **kwargs)

        def database_command(query):
            return docker('exec', container, 'psql', '-U', 'postgres', '-d', 'pmman',
                          '-At', '-v', 'ON_ERROR_STOP=1', '-c', query,
                          capture_output=True, text=True).stdout.strip()

        def mapped_run(args, **kwargs):
            args = list(args)
            if args[-2:] == ['stop', 'master']:
                return subprocess.CompletedProcess(args, 0, '', '')
            # Only redirect this module's Compose postgres exec to the
            # dedicated container. No production Compose invocation occurs.
            self.assertEqual(args[:2], ['docker', 'compose'])
            marker = args.index('exec')
            self.assertEqual(args[marker:marker + 3], ['exec', '-T', 'postgres'])
            return real_run(['docker', 'exec', '-i', container, *args[marker + 3:]], **kwargs)

        docker('run', '--rm', '-d', '--name', container, '--network', 'none',
               '--tmpfs', '/var/lib/postgresql/data',
               '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', '-e', 'POSTGRES_DB=pmman',
               'postgres:16.4-alpine', capture_output=True)
        try:
            deadline = time.monotonic() + 30
            while True:
                ready = real_run(['docker', 'exec', container, 'pg_isready',
                                  '-U', 'postgres', '-d', 'pmman', '-h', '127.0.0.1'],
                                 capture_output=True)
                if ready.returncode == 0:
                    break
                if time.monotonic() >= deadline:
                    self.fail('test PostgreSQL did not become ready')
                time.sleep(0.2)
            database_command('CREATE TABLE original(id integer PRIMARY KEY); INSERT INTO original VALUES(7); '
                             'CREATE TABLE schema_migrations(version bigint PRIMARY KEY); '
                             'INSERT INTO schema_migrations VALUES(4)')
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                with patch.object(release.subprocess, 'run', side_effect=mapped_run):
                    release.backup_postgres(root, 'postgres', 'pmman')
                self.assertEqual(json.loads((root / 'postgres-tables.json').read_text()),
                                 ['original', 'schema_migrations'])
                database_command('CREATE TABLE added(id integer REFERENCES original); '
                                 'INSERT INTO added VALUES(7); INSERT INTO original VALUES(8)')
                schema = pathlib.Path(__file__).resolve().parents[2] / 'server/src/address_pools.sql'
                database_command(schema.read_text())
                with patch.object(release.subprocess, 'run', side_effect=mapped_run):
                    release.restore_postgres(root, 'postgres', 'pmman')
                self.assertEqual(database_command('SELECT id FROM original ORDER BY id'), '7')
                self.assertEqual(database_command("SELECT count(*) FROM pg_tables "
                                                 "WHERE schemaname='public' AND tablename='added'"), '0')
                self.assertEqual(database_command('SELECT max(version) FROM schema_migrations'), '4')
                self.assertEqual(database_command("SELECT count(*) FROM pg_tables WHERE schemaname='public'"), '2')
        finally:
            docker('rm', '-f', container, capture_output=True)


if __name__ == '__main__':
    unittest.main()
