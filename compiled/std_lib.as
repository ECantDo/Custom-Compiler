initialize_heap:
	li	1 t0
	str	t0 0(zero)
	ret

malloc:
	lod	t1 0(zero)
	add	t1 a0 t2
	li	1 t0
	li	127 t3
	bleu	t2 t3 malloc_if.end
malloc_common.ret:
	mv	t0 a0
	ret
malloc_if.end:
	str	t2 0(zero)
	mv	t1 t0
	j	malloc_common.ret

free:
	li	1 t0
	str	t0 0(zero)
	ret
