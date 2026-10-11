package main

import (
	"fmt"
	"sync"
)

func collatz(n int64) int64 {
	steps := int64(0)
	for n != 1 {
		if n%2 == 0 {
			n /= 2
		} else {
			n = 3*n + 1
		}
		steps++
	}
	return steps
}

func main() {
	jobs, results := make(chan int64, 1024), make(chan int64, 1024)
	var wg sync.WaitGroup
	for w := 0; w < 8; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := range jobs {
				results <- collatz(j)
			}
		}()
	}
	go func() {
		for i := int64(1); i <= 200000; i++ {
			jobs <- i
		}
		close(jobs)
	}()
	go func() { wg.Wait(); close(results) }()
	total := int64(0)
	for r := range results {
		total += r
	}
	fmt.Println(total)
}
