import subprocess
import warnings

import paramiko

# ssh-ed25519 signature verification is broken
paramiko.Transport._preferred_keys = tuple(
    filter(lambda s: s != "ssh-ed25519", paramiko.Transport._preferred_keys)
)

# paramiko raises some DeprecationWarnings we don't care about
warnings.filterwarnings("ignore", module="paramiko.*")


class SSHCommandError(subprocess.CalledProcessError):
    def __str__(self):
        return (
            super().__str__()
            + (f"\nSTDERR:\n{self.stderr}\n" if self.stderr else "")
            + (f"\nOUTPUT:\n{self.output}\n" if self.output else "")
        )


class SSHClient(paramiko.SSHClient):
    def __init__(self):
        super().__init__()
        # ignore SSH host keys
        self.set_missing_host_key_policy(paramiko.client.MissingHostKeyPolicy)

    def ext_exec_command(self, command):
        # paramiko.SSHClient currently gives no access to a command's exit
        # status.
        chan = self._transport.open_session()
        chan.exec_command(command)

        stdout = chan.makefile("r")
        stderr = chan.makefile_stderr("r")
        exit_status = chan.recv_exit_status()
        stdout_data = stdout.read()
        stderr_data = stderr.read()

        if exit_status == 0:
            return (stdout_data, stderr_data)
        else:
            raise SSHCommandError(
                cmd=command,
                returncode=exit_status,
                output=stdout_data,
                stderr=stderr_data,
            )
