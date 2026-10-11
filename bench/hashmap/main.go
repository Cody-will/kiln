package main

import "fmt"

func main() {
	m := map[int64]int64{}
	n := int64(1000000)
	for i := int64(0); i < n; i++ {
		m[i*7] = i
	}
	hits := int64(0)
	for i := int64(0); i < 2*n; i++ {
		if v, ok := m[i]; ok {
			hits += v
		}
	}
	for i := int64(0); i < n/2; i++ {
		delete(m, i*14)
	}
	s := map[string]int64{}
	for i := int64(0); i < 200000; i++ {
		s[fmt.Sprintf("key-%d", i)] = i
	}
	shits := 0
	for i := 0; i < 200000; i++ {
		if _, ok := s[fmt.Sprintf("key-%d", i*3)]; ok {
			shits++
		}
	}
	fmt.Println(hits, len(m), shits)
}
