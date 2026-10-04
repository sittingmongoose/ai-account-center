"""Released-only native callback factory; false release returns before any I/O."""
from pathlib import Path
import re

from native_status_omitted_counters import NATIVE_SHA256
from native_status_publisher import NativePublisherCheck,pin_native_file
from native_status_service import NativeStatusService


def create_native_status_factory(*,release,binary,home,status_path,python_path,
                                helper_path,original_command_file,
                                read_profile=None,read_snapshot=None,publisher_validator=None):
    if (type(release) is not dict or release.get('nativeActivationReleased') is not True or
            release.get('nativeVersion')!='1.2.16' or release.get('nativeSha256')!=NATIVE_SHA256 or
            type(release.get('nativeProofReceiptSha256')) is not str or
            not re.fullmatch('[a-f0-9]{64}',release['nativeProofReceiptSha256'])):
        return None
    if publisher_validator is None:
        publisher_validator=NativePublisherCheck(binary,pin_native_file(binary,NATIVE_SHA256))
    if read_profile is None:
        def read_profile():
            from auth_platforms import UbuntuFileStore
            from native_credential_worker import execute
            return execute({'operation':'bind-current'},UbuntuFileStore(Path(home)))
    if read_snapshot is None:
        from auth_platforms import UbuntuFileStore
        store=UbuntuFileStore(Path(home))  # Constructor connects to no bus/store.
        read_snapshot=store.file.read_current
    return lambda broker:NativeStatusService(broker,path=status_path,python_path=python_path,
        helper_path=helper_path,original_command_file=original_command_file,
        read_profile=read_profile,read_snapshot=read_snapshot,publisher_validator=publisher_validator,
        producer_contract_approved=True)
