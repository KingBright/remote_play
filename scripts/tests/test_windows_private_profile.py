import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import copy
import unittest
import windows_private_profile as w

USER='S-1-5-21-1-2-3-1007'
def baseline():
    return [{'type':0,'flags':0x13,'mask':w.FULL,'sid':sid} for sid in [w.SYSTEM,w.ADMINISTRATORS,USER]]


class PermissionPlanTests(unittest.TestCase):
    def test_remove_only_inherited_extra_grants_preserve_known_rights(self):
        rows=baseline()+[{'type':0,'flags':0x10,'mask':0x1200a9,'sid':w.EXTRA},
            {'type':0,'flags':0x1b,'mask':0xa0000000,'sid':w.EXTRA}]
        before=copy.deepcopy(rows);after,removed=w.derive_private_rows(rows,USER)
        self.assertEqual(rows,before)
        self.assertEqual(after,[dict(x,flags=3) for x in baseline()])
        self.assertEqual(removed,rows[-2:])

    def test_never_whitelist_capability_family(self):
        rows=baseline()+[{'type':0,'flags':0x10,'mask':1,'sid':'S-1-15-3-999'}]
        after,removed=w.derive_private_rows(rows,USER)
        self.assertEqual(len(after),3);self.assertEqual(len(removed),1)

    def test_explicit_foreign_grant_requires_separate_review(self):
        with self.assertRaises(RuntimeError):
            w.derive_private_rows(baseline()+[{'type':0,'flags':0,'mask':1,'sid':w.EXTRA}],USER)

    def test_denies_remain_and_never_become_allows(self):
        denial={'type':1,'flags':0x10,'mask':2,'sid':w.EXTRA}
        after,removed=w.derive_private_rows([denial]+baseline(),USER)
        self.assertEqual(after[0],dict(denial,flags=0));self.assertEqual(removed,[])

    def test_no_missing_system_or_owner_rights_invented(self):
        for missing in [USER,w.SYSTEM,w.ADMINISTRATORS]:
            with self.subTest(missing=missing),self.assertRaises(RuntimeError):
                w.derive_private_rows([x for x in baseline() if x['sid']!=missing],USER)

    def test_partial_rights_not_silently_upgraded(self):
        rows=baseline();rows[-1]['mask']=0x1200a9
        with self.assertRaises(RuntimeError):w.derive_private_rows(rows,USER)

    def test_unknown_ace_semantics_refused(self):
        for row in [dict(type=5,flags=0,mask=1,sid=USER),dict(type=0,flags=0x40,mask=1,sid=USER)]:
            with self.subTest(row=row),self.assertRaises(RuntimeError):
                w.derive_private_rows(baseline()+[row],USER)

    def test_only_inherited_bit_removed_from_known_aces(self):
        rows=baseline();rows[0]['flags']=0x1f
        after,_=w.derive_private_rows(rows,USER)
        self.assertEqual(after[0]['flags'],0x0f)
        self.assertEqual(after[0]['mask'],w.FULL)

    def test_new_directory_descriptor_has_exact_three_principals(self):
        s=w.private_sddl(USER)
        self.assertTrue(s.startswith('O:'+USER+'D:P'))
        self.assertEqual(s.count('(A;OICI;FA;;;'),3)
        self.assertNotIn(w.EXTRA,s)

    def test_proposed_dacl_keeps_all_denials_and_flags(self):
        rows=[dict(type=1,flags=8,mask=2,sid=w.EXTRA)]+[dict(x,flags=3) for x in baseline()]
        s=w.rows_dacl(rows)
        self.assertTrue(s.startswith('D:P(D;IO;0x2;;;'))
        self.assertEqual(s.count(';0x1f01ff;;;'),3)


if __name__=='__main__':unittest.main()
