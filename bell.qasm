OPENQASM 2.0;
include "qelib1.inc";
qreg q[2];
creg c[2];
// Circuit: bell_state (IBM native basis: rz, sx, x, cx, measure)
rz(3.141592653589793) q[0];
sx q[0];
rz(-1.5707963267948966) q[0];
sx q[0];
rz(1.5707963267948966) q[0];
cx q[0], q[1];
measure q[1] -> c[1];
rz(1.5707963267948966) q[0];
measure q[0] -> c[0];
