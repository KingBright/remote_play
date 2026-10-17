import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ctypes
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from windows_process_lifecycle import stop_verified


class Function:
    def __init__(self, callback):
        self.call = Mock(side_effect=callback)
    def __call__(self, *args):
        return self.call(*args)


def kernel(path='/expected', waited=0):
    def image(handle, flags, buffer, length):
        buffer.value = path
        return True
    return SimpleNamespace(OpenProcess=Function(lambda *a: 77),
        QueryFullProcessImageNameW=Function(image), TerminateProcess=Function(lambda *a: True),
        WaitForSingleObject=Function(lambda *a: waited), CloseHandle=Function(lambda *a: True))


class HeldHandleTests(unittest.TestCase):
    def test_other_image_never_terminated(self):
        k=kernel('/other');i=SimpleNamespace(native=Mock())
        with self.assertRaises(RuntimeError):
            stop_verified(i,12,Path('/expected'),'a'*64,kernel=k)
        k.TerminateProcess.call.assert_not_called()
        i.native.assert_not_called()
        k.CloseHandle.call.assert_called_once_with(77)

    def test_failed_native_identity_never_terminated(self):
        k=kernel();i=SimpleNamespace(native=Mock(side_effect=RuntimeError('wrong identity')))
        with self.assertRaises(RuntimeError):
            stop_verified(i,12,Path('/expected'),'a'*64,kernel=k)
        k.TerminateProcess.call.assert_not_called()
        k.CloseHandle.call.assert_called_once_with(77)

    def test_same_handle_held_through_verify_stop_and_wait(self):
        k=kernel();i=SimpleNamespace(native=Mock())
        stop_verified(i,12,Path('/expected'),'a'*64,kernel=k)
        i.native.assert_called_once_with(12,Path('/expected'),'a'*64,None)
        k.TerminateProcess.call.assert_called_once_with(77,0)
        k.WaitForSingleObject.call.assert_called_once_with(77,15000)
        k.CloseHandle.call.assert_called_once_with(77)

    def test_unconfirmed_exit_is_failure(self):
        k=kernel(waited=258);i=SimpleNamespace(native=Mock())
        with self.assertRaises(RuntimeError):
            stop_verified(i,12,Path('/expected'),'a'*64,kernel=k)
        k.CloseHandle.call.assert_called_once_with(77)


if __name__=='__main__':
    unittest.main()
