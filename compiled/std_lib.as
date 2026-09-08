initialize_heap:
	li	1 t4
	str	t4 0(zero)
	ret

malloc:
	lod	s2 0(zero)
	add	s2 a0 s1
	li	127 t4
	bleu	s1 t4 malloc_if.end
	li	1 s0
malloc_common.ret:
	mv	s0 a0
	ret
malloc_if.end:
	str	s1 0(zero)
	mv	s2 s0
	j	malloc_common.ret

free:
	li	1 t4
	str	t4 0(zero)
	ret
