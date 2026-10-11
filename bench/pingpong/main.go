package main

import "fmt"

func main() {
	ping, pong := make(chan int64), make(chan int64)
	done := make(chan struct{})
	go func() {
		for v := range ping {
			pong <- v + 1
		}
		close(done)
	}()
	v := int64(0)
	for i := 0; i < 500000; i++ {
		ping <- v
		v = <-pong
	}
	close(ping)
	<-done
	fmt.Println(v)
}
