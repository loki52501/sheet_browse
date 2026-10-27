"""Tests for check_recipes.py (T-27).  python -m unittest discover -s scripts/recipes -v"""
import copy
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_recipes as cr  # noqa: E402

PATH = Path("tools/mail.google.com/send.recipe.yaml")
GOOD = {
    "name": "send",
    "domain": "mail.google.com",
    "kind": "recipe",
    "description": "Compose and send",
    "match": ["/mail/*"],
    "args": {"to": {"type": "string", "required": True}, "count": {"type": "int", "default": 5}},
    "steps": [
        {"tool": "navigate", "args": {"url": "https://mail.google.com/mail/u/0/#inbox?compose=new"}},
        {"tool": "wait", "args": {"for": "selector", "value": "[role=dialog]"}},
        {"tool": "act", "target": {"role": "combobox", "name": "To recipients"},
         "args": {"kind": "fill", "value": "{to}"}},
        {"tool": "act", "args": {"kind": "press", "key": "Tab"}},
        {"tool": "grep", "args": {"over": "ax", "pattern": "row", "limit": "{count}"},
         "returns": "rows"},
    ],
    "expects": {"text": "Message sent"},
}


def bad(mutate, path=PATH, aliases=()):
    doc = copy.deepcopy(GOOD)
    mutate(doc)
    return cr.check_doc(doc, path, aliases)


class RealRecipes(unittest.TestCase):
    def test_every_recipe_in_tools_passes(self):
        found = cr.recipes()
        names = {(p.parent.name, p.name) for p in found}
        for want in [("mail.google.com", f"{n}.recipe.yaml") for n in ("inbox", "thread", "draft", "send")] + [
            ("calendar.google.com", "events.recipe.yaml"), ("calendar.google.com", "create.recipe.yaml"),
            ("lu.ma", "register.recipe.yaml"),
        ]:
            self.assertIn(want, names)
        for p in found:
            self.assertEqual(cr.check_file(p), [], p)

    def test_lu_ma_alias_covers_the_luma_com_redirect(self):
        self.assertIn("luma.com", cr.site_aliases(cr.ROOT / "tools" / "lu.ma"))


class Rules(unittest.TestCase):
    def test_good_passes(self):
        self.assertEqual(cr.check_doc(copy.deepcopy(GOOD), PATH), [])

    def assertFlags(self, problems, needle):
        self.assertTrue(any(needle in p for p in problems), f"{needle!r} not in {problems}")

    def test_name_must_equal_stem_and_be_header_safe(self):
        self.assertFlags(bad(lambda d: d.update(name="other")), "file stem")
        self.assertFlags(bad(lambda d: d.update(name="Send")), "[a-z0-9_]+")
        self.assertFlags(bad(lambda d: d.update(name="se-nd")), "[a-z0-9_]+")

    def test_domain_must_equal_folder(self):
        self.assertFlags(bad(lambda d: d.update(domain="google.com")), "folder")
        self.assertFlags(bad(lambda d: d.update(domain="../etc")), "site folder")

    def test_kind_and_description(self):
        self.assertFlags(bad(lambda d: d.update(kind="planner")), "kind")
        self.assertFlags(bad(lambda d: d.update(description=" ")), "description")

    def test_unknown_keys(self):
        self.assertFlags(bad(lambda d: d.update(extra=1)), "unknown key 'extra'")
        self.assertFlags(bad(lambda d: d["steps"][0].update(sleep=1)), "unknown key 'sleep'")
        self.assertFlags(bad(lambda d: d["steps"][2]["target"].update(css=".x")), "unknown key 'css'")

    def test_match_rules(self):
        for p, why in [("https://evil.com/*", "scheme"), ("//evil.com/*", "//"), ("/a/../b", ".."),
                       ("/a\\b", "backslash"), ("/%2e%2e/x", "percent"), ("mail/*", "start with /")]:
            self.assertFlags(bad(lambda d, p=p: d.update(match=[p])), why)
        self.assertFlags(bad(lambda d: d.update(match=[])), "non-empty")

    def test_arg_specs(self):
        self.assertFlags(bad(lambda d: d["args"]["count"].update(type="float")), "string, int or bool")
        self.assertFlags(bad(lambda d: d["args"]["count"].update(default="5")), "is not a int")
        self.assertFlags(bad(lambda d: d["args"]["count"].update(default=True)), "is not a int")
        self.assertFlags(bad(lambda d: d["args"]["to"].update(default="x")), "required and has a default")
        self.assertFlags(bad(lambda d: d["args"]["to"].update(help="x")), "unknown key 'help'")

    def test_placeholders_only_for_declared_args(self):
        self.assertFlags(bad(lambda d: d["steps"][2]["args"].update(value="{too}")), "{too}")
        self.assertFlags(bad(lambda d: d.update(expects={"text": "{nope}"})), "{nope}")

    def test_declared_arg_must_be_used(self):
        self.assertFlags(bad(lambda d: d["args"].update(spare={"type": "string"})), "never used")

    def test_no_raw_ref(self):
        self.assertFlags(bad(lambda d: d["steps"][2]["target"].update(name="e12")), "raw ref")
        self.assertFlags(bad(lambda d: d["steps"][2]["args"].update(ref="e7")), "raw ref key")
        self.assertFlags(bad(lambda d: d["steps"][2]["args"].update(value='x [ref=e3]')), "raw ref")

    def test_no_fixed_sleep(self):
        self.assertFlags(bad(lambda d: d["steps"][1]["args"].update({"for": "time", "value": 2000})), "fixed sleep")
        self.assertFlags(bad(lambda d: d["steps"][1]["args"].pop("for")), "fixed sleep")

    def test_tools_are_browseros_recipe_tools(self):
        for t in ("run", "evaluate", "request_human_help", "sleep"):
            self.assertFlags(bad(lambda d, t=t: d["steps"][1].update(tool=t)), "not a BrowserOS recipe tool")

    def test_target_shape(self):
        self.assertFlags(bad(lambda d: d["steps"][2].pop("target")), "has no target")
        self.assertFlags(bad(lambda d: d["steps"][3].update(target={"role": "button"})), "takes no target")
        self.assertFlags(bad(lambda d: d["steps"][1].update(target={"role": "button"})), "has a target but")
        self.assertFlags(bad(lambda d: d["steps"][2]["target"].pop("role")), "no role")
        self.assertFlags(bad(lambda d: d["steps"][3]["args"].pop("key")), "no key")
        self.assertFlags(bad(lambda d: d["steps"][2]["args"].update(kind="dialog_accept")), "not allowed")

    def test_navigate_stays_on_site(self):
        for url, why in [("https://evil.com/x", "leaves"), ("http://mail.google.com/", "not https"),
                         ("https://{to}/x", "placeholder before the path"),
                         ("https://mail.google.com.evil.com/", "leaves"),
                         ("https://u:p@mail.google.com/", "credentials"),
                         ("javascript:alert(1)", "not an absolute"), ("mail.google.com/x", "not an absolute")]:
            self.assertFlags(bad(lambda d, u=url: d["steps"][0]["args"].update(url=u)), why)
        lu = Path("tools/lu.ma/register.recipe.yaml")

        def to_luma(d):
            d.update(name="register", domain="lu.ma")
            d["steps"][0]["args"]["url"] = "https://luma.com/x"
        self.assertEqual(bad(to_luma, lu, ["luma.com"]), [])
        self.assertFlags(bad(to_luma, lu, []), "leaves")

    def test_vault_only_as_bare_key_in_act_text(self):
        def put(d, v):
            d["steps"][2]["args"]["value"] = v
        self.assertEqual(bad(lambda d: (put(d, "@vault(gmail_pw)"), d["args"].pop("to"))), [])
        self.assertFlags(bad(lambda d: d.update(expects={"text": "@vault(k)"})), "only act text/value")
        self.assertFlags(bad(lambda d: d["steps"][0]["args"].update(
            url="https://mail.google.com/@vault(k)")), "only act text/value")
        self.assertFlags(bad(lambda d: (put(d, "x @vault(k) {to}"))), "bare @vault(key)")

    def test_no_password_or_key_shaped_string(self):
        def pw(d):
            d["steps"][2]["target"] = {"role": "textbox", "name": "Password"}
            d["steps"][2]["args"]["value"] = "{to}"
        self.assertFlags(bad(pw), "without @vault(key)")
        self.assertFlags(bad(lambda d: d["args"].update(password={"type": "string"})), "secret-looking key")
        self.assertFlags(bad(lambda d: d.update(description="token ghp_" + "a" * 36)), "key-shaped")

    def test_returns_and_expects_shape(self):
        self.assertFlags(bad(lambda d: d["steps"][4].update(returns="Bad Name")), "returns")
        self.assertFlags(bad(lambda d: d.update(expects={"url": "x"})), "expects")

    def test_check_file_reads_yaml_and_site_aliases(self):
        with tempfile.TemporaryDirectory() as t:
            folder = Path(t) / "lu.ma"
            folder.mkdir()
            (folder / "_site.yaml").write_text("aliases: [luma.com]\n", encoding="utf-8")
            f = folder / "register.recipe.yaml"
            f.write_text(
                "name: register\ndomain: lu.ma\nkind: recipe\ndescription: r\nmatch: ['/*']\n"
                "args: {event: {type: string, required: true}}\n"
                "steps:\n  - tool: navigate\n    args: {url: 'https://luma.com/{event}'}\n",
                encoding="utf-8")
            self.assertEqual(cr.check_file(f), [])
            f.write_text("name: [unclosed\n", encoding="utf-8")
            self.assertFlags(cr.check_file(f), "YAML")
            self.assertFlags(cr.check_file(folder / "_site.yaml"), "does not end with")


if __name__ == "__main__":
    unittest.main()
