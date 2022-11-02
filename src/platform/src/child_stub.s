  .section .text

sentinel_child_stub:
  movq $39, %rax  # sys_getpid
  syscall

  movq %rax, %rdi
  movq $62, %rax  # sys_kill
  movq $19, %rsi  # sigstop
  syscall  # some syscall will be injected in this `syscall` call

done:
  int $3
  jmp done

  .globl addr_of_stub
addr_of_stub:
  lea sentinel_child_stub(%rip), %rax
  ret
