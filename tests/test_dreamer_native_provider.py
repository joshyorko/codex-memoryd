import importlib.util
import io
import json
import sys
import types
import unittest
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from unittest.mock import Mock, patch

@contextmanager
def redirect_stdin(stream):
    with patch('sys.stdin', stream):
        yield

ROOT = __import__('pathlib').Path(__file__).parents[1]
SPEC = importlib.util.spec_from_file_location('dreamer_native_provider', ROOT / 'scripts/dreamer_native_provider.py')
MOD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MOD)


class NativeProviderTests(unittest.TestCase):
    def setUp(self):
        self.create = Mock()
        response = types.SimpleNamespace(
            choices=[types.SimpleNamespace(message=types.SimpleNamespace(content='[{"ok":true}]'))]
        )
        self.create.return_value = response
        client = types.SimpleNamespace(chat=types.SimpleNamespace(completions=types.SimpleNamespace(create=self.create)))
        aux = types.ModuleType('agent.auxiliary_client')
        aux.resolve_provider_client = Mock(return_value=(client, 'gpt-5.6-luna'))
        agent = types.ModuleType('agent')
        agent.auxiliary_client = aux
        self.old = {name: sys.modules.get(name) for name in ('agent', 'agent.auxiliary_client')}
        sys.modules['agent'] = agent
        sys.modules['agent.auxiliary_client'] = aux

    def tearDown(self):
        for name, value in self.old.items():
            if value is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = value

    def invoke(self, request, args=None):
        incoming = io.TextIOWrapper(io.BytesIO(json.dumps(request).encode()))
        output = io.BytesIO()
        out, err = io.TextIOWrapper(output, write_through=True), io.StringIO()
        with redirect_stdin(incoming), redirect_stdout(out), redirect_stderr(err):
            code = MOD.main(args or ['--provider', 'openai-codex', '--model', 'gpt-5.6-luna'])
        return code, output.getvalue().decode(), err.getvalue()

    def test_tools_empty_single_inference_and_pinned_resolution(self):
        code, out, err = self.invoke({'model': 'gpt-5.6-luna', 'tools': [], 'input': 'synthetic'})
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(json.loads(out)['choices'][0]['message']['content']), [{'ok': True}])
        self.create.assert_called_once()
        kwargs = self.create.call_args.kwargs
        self.assertEqual(kwargs['tools'], [])
        self.assertEqual(kwargs['model'], 'gpt-5.6-luna')
        self.assertEqual(kwargs['reasoning_effort'], 'low')

    def test_rejects_tools_executable_and_model_mismatch(self):
        for request in (
            {'model': 'gpt-5.6-luna', 'tools': [{'type': 'function'}], 'input': 'x'},
            {'model': 'gpt-5.6-luna', 'executable': 'sh', 'input': 'x'},
            {'model': 'other', 'tools': [], 'input': 'x'},
        ):
            code, _, err = self.invoke(request)
            self.assertNotEqual(code, 0)
            self.assertIn('does not match' if request['model'] != 'gpt-5.6-luna' else 'not permitted', err)
        self.create.assert_not_called()

    def test_auth_failure_is_nonzero_without_fallback(self):
        sys.modules['agent.auxiliary_client'].resolve_provider_client.side_effect = RuntimeError('auth failed')
        code, out, err = self.invoke({'model': 'gpt-5.6-luna', 'tools': [], 'input': 'x'})
        self.assertNotEqual(code, 0)
        self.assertEqual(out, '')
        self.assertIn('provider inference failed', err)
        self.assertNotIn('auth failed', err)

    def test_no_implicit_provider_or_model_fallback(self):
        code, _, _ = self.invoke({'model': 'gpt-5.6-luna', 'tools': [], 'input': 'x'}, ['--provider', 'openai-codex', '--model', 'gpt-5.6-luna'])
        self.assertEqual(code, 0)
        with self.assertRaises(SystemExit):
            MOD.main([])


if __name__ == '__main__':
    unittest.main()
