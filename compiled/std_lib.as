initialize_heap:
	li	t0, 1
	str	t0 0(zero)
	ret
malloc:
	lod	t1 0(zero)
	add	t1 a0 t2
	li	t0, 1
	li	t3, 127
	bgtu	t2 t3 malloc_common.ret
malloc_common.ret:
	mv	t0 a0
	ret
malloc_if.end:
	str	t2 0(zero)
	mv	t1 t0
	j	malloc_common.ret
free:
	li	t0, 1
	str	t0 0(zero)
	ret

