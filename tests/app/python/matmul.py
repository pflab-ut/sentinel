import os
os.environ["OMP_NUM_THREADS"] = "1"
os.environ["OPENBLAS_NUM_THREADS"] = "1"
os.environ["MKL_NUM_THREADS"] = "1"
os.environ["VECLIB_MAXIMUM_THREADS"] = "1"
os.environ["NUMEXPR_NUM_THREADS"] = "1"

import numpy as np

np.random.seed(0)

size = 1024
A, B = np.random.random((size, size)), np.random.random((size, size))

C = np.dot(A, B)
print(A.shape, B.shape, C.shape)
print('matmul result:', A)
