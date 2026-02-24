import { clsx } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs) {
	return twMerge(clsx(inputs))
}

export function formatRelativeTime(isoString) {
	if (!isoString) return ''
	const now = new Date()
	const time = new Date(isoString)
	const diffMs = now - time
	const minutes = Math.floor(diffMs / 60000)
	const hours = Math.floor(diffMs / 3600000)
	const days = Math.floor(diffMs / 86400000)
	if (days > 0) return days === 1 ? '1 day ago' : `${days} days ago`
	if (hours > 0) return hours === 1 ? '1 hour ago' : `${hours} hours ago`
	if (minutes > 0) return minutes === 1 ? '1 minute ago' : `${minutes} minutes ago`
	return 'just now'
}
